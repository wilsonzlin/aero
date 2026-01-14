use std::{
    collections::HashMap,
    fmt, fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use crate::range_set::{ByteRange, RangeSet};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, ACCEPT_RANGES, CACHE_CONTROL,
    CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{oneshot, Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;
use url::Url;

const META_FILE_NAME: &str = "streaming-cache-meta.json";
const CHUNKS_DIR_NAME: &str = "chunks";
const CACHE_FILE_NAME: &str = "cache.bin";

pub const DEFAULT_SECTOR_SIZE: u64 = crate::SECTOR_SIZE as u64;
pub const DEFAULT_CHUNK_SIZE: u64 = 1024 * 1024; // 1MiB

const CACHE_META_VERSION: u32 = 2;

// Bound per-request allocation size when streaming from untrusted servers. `StreamingDisk`
// downloads whole chunks into memory before persisting them to the cache backend, so the
// chunk size must remain reasonably small.
// 64 MiB.
const MAX_STREAMING_CHUNK_SIZE: u64 = 64 * 1024 * 1024;
// Bound best-effort sequential prefetch. When `read_ahead_chunks` is misconfigured (or attacker
// controlled), prefetching too far ahead can cause large background downloads and cache growth.
//
// We cap both the number of chunks and the total prefetched byte volume to keep the work bounded
// even when `chunk_size` is very small.
const MAX_STREAMING_READ_AHEAD_CHUNKS: u64 = 1024;
// 512 MiB.
const MAX_STREAMING_READ_AHEAD_BYTES: u64 = 512 * 1024 * 1024;
// Bound retry and concurrency knobs for untrusted config. Very large values can cause pathological
// background work, extremely long retry loops, or large in-flight allocations.
const MAX_STREAMING_MAX_RETRIES: usize = 32;
const MAX_STREAMING_MAX_CONCURRENT_FETCHES: usize = 128;
// Upper bound on total in-flight bytes across concurrent chunk downloads:
// `max_concurrent_fetches * min(chunk_size, total_size)`.
// 512 MiB.
const MAX_STREAMING_INFLIGHT_BYTES: u64 = 512 * 1024 * 1024;

fn require_no_transform_cache_control(
    headers: &HeaderMap,
    label: &str,
) -> Result<(), StreamingDiskError> {
    // Defence-in-depth against intermediary transforms. Disk streaming reads bytes by offset; any
    // transform (compression, content rewriting, etc) can break deterministic byte addressing.
    //
    // Keep this consistent with the browser clients and conformance tool expectations.
    let cache_control = headers
        .get(CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            StreamingDiskError::Protocol(format!(
                "{label}: missing Cache-Control header (expected include no-transform)"
            ))
        })?;
    let has_no_transform = cache_control
        .split(',')
        .map(|t| t.trim())
        .any(|t| t.eq_ignore_ascii_case("no-transform"));
    if !has_no_transform {
        return Err(StreamingDiskError::Protocol(format!(
            "{label}: Cache-Control missing no-transform: {cache_control}"
        )));
    }
    Ok(())
}

#[derive(Debug, Error, Clone)]
pub enum StreamingDiskError {
    #[error("remote server does not support HTTP Range requests")]
    RangeNotSupported,

    #[error("remote request failed with HTTP status {status}")]
    HttpStatus { status: u16 },

    #[error("remote request failed: {0}")]
    Http(String),

    #[error("unexpected remote response: {0}")]
    Protocol(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("serialization error: {0}")]
    Serde(String),

    #[error("integrity check failed for chunk {chunk_index}: expected {expected} got {actual}")]
    Integrity {
        chunk_index: u64,
        expected: String,
        actual: String,
    },

    #[error("remote validator mismatch (expected {expected:?}, got {actual:?})")]
    ValidatorMismatch {
        expected: Option<String>,
        actual: Option<String>,
    },

    #[error("operation cancelled")]
    Cancelled,

    #[error("out of bounds access: offset {offset} len {len} size {size}")]
    OutOfBounds { offset: u64, len: u64, size: u64 },

    #[error("URL must be absolute: {0}")]
    UrlNotAbsolute(String),
}

impl From<std::io::Error> for StreamingDiskError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for StreamingDiskError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct ChunkManifest {
    pub chunk_size: u64,
    pub sha256: Vec<[u8; 32]>,
}

impl ChunkManifest {
    pub fn sha256_for_chunk(&self, chunk_index: u64) -> Option<[u8; 32]> {
        let idx: usize = chunk_index.try_into().ok()?;
        self.sha256.get(idx).copied()
    }
}

fn alloc_zeroed(len: usize) -> Result<Vec<u8>, StreamingDiskError> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let mut buf = Vec::new();
    buf.try_reserve_exact(len)
        .map_err(|_| StreamingDiskError::Io(format!("allocation failed for {len} bytes")))?;
    buf.resize(len, 0);
    Ok(buf)
}

#[derive(Debug, Clone)]
pub struct StreamingDiskOptions {
    /// Caching unit for the remote image. All range fetches are chunk-aligned.
    pub chunk_size: u64,
    /// How many chunks to prefetch when sequential reads are detected.
    pub read_ahead_chunks: u64,
    /// Maximum concurrent HTTP range fetches.
    pub max_concurrent_fetches: usize,
    /// Maximum retries for a failed HTTP range fetch.
    pub max_retries: usize,
    /// Optional per-chunk integrity verification.
    pub manifest: Option<ChunkManifest>,
}

impl Default for StreamingDiskOptions {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            read_ahead_chunks: 2,
            max_concurrent_fetches: 4,
            max_retries: 4,
            manifest: None,
        }
    }
}

/// Persistent cache backend used for storing fetched chunks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamingCacheBackend {
    /// Store chunks as individual files under `cache_dir/chunks/`.
    Directory,
    /// Store chunks in a sparse file at `cache_dir/cache.bin`.
    #[default]
    SparseFile,
}

#[derive(Clone)]
pub struct StreamingDiskConfig {
    pub url: Url,
    pub cache_dir: PathBuf,
    /// Additional headers applied to all HTTP requests (`HEAD` + `GET Range`).
    ///
    /// This is intended for auth (`Authorization`, `Cookie`, etc). The URL is intentionally
    /// excluded from the persistent cache identity, and these headers are *not* persisted.
    pub request_headers: Vec<(String, String)>,
    /// Optional stable validator for the image (e.g. ETag).
    ///
    /// When unset, `StreamingDisk` will attempt to use the server-provided `ETag`
    /// from `HEAD`/`GET` and persist it as the cache identity.
    pub validator: Option<String>,
    pub cache_backend: StreamingCacheBackend,
    pub options: StreamingDiskOptions,
}

impl fmt::Debug for StreamingDiskConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `url` and `request_headers` may contain sensitive auth material (e.g. signed URLs,
        // `Authorization` headers). Redact by default to avoid accidental leakage in logs.
        let url = redact_url_for_logs(&self.url);

        let header_names: Vec<&str> = self
            .request_headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();

        f.debug_struct("StreamingDiskConfig")
            .field("url", &url)
            .field("cache_dir", &self.cache_dir)
            .field("request_headers", &header_names)
            .field("validator", &self.validator)
            .field("cache_backend", &self.cache_backend)
            .field("options", &self.options)
            .finish()
    }
}

impl StreamingDiskConfig {
    pub fn new(url: Url, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            url,
            cache_dir: cache_dir.into(),
            request_headers: Vec::new(),
            validator: None,
            cache_backend: StreamingCacheBackend::default(),
            options: StreamingDiskOptions::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStatus {
    pub total_size: u64,
    pub cached_bytes: u64,
    pub cached_ranges: Vec<ByteRange>,
    pub chunk_size: u64,
    pub validator: Option<String>,
}

#[derive(Default)]
pub struct StreamingTelemetry {
    pub bytes_downloaded: AtomicU64,
    pub range_requests: AtomicU64,
    /// Number of chunks served entirely from the local cache (no HTTP fetch was required).
    pub cache_hit_chunks: AtomicU64,
    /// Number of chunk fetches initiated (deduplicated across concurrent readers).
    pub cache_miss_chunks: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingTelemetrySnapshot {
    pub bytes_downloaded: u64,
    pub range_requests: u64,
    pub cache_hit_chunks: u64,
    pub cache_miss_chunks: u64,
}

impl StreamingTelemetry {
    pub fn snapshot(&self) -> StreamingTelemetrySnapshot {
        StreamingTelemetrySnapshot {
            bytes_downloaded: self.bytes_downloaded.load(Ordering::Relaxed),
            range_requests: self.range_requests.load(Ordering::Relaxed),
            cache_hit_chunks: self.cache_hit_chunks.load(Ordering::Relaxed),
            cache_miss_chunks: self.cache_miss_chunks.load(Ordering::Relaxed),
        }
    }
}

/// Persistent chunk cache interface used by [`StreamingDisk`].
///
/// This is an internal storage abstraction for the HTTP range streaming/cache subsystem. It is
/// **not** intended to be a general-purpose “disk trait” for device/controller code.
///
/// For synchronous disk image formats and controller/device integration, prefer the canonical
/// `aero_storage::{StorageBackend, VirtualDisk}` traits instead.
///
/// See `docs/20-storage-trait-consolidation.md`.
pub trait ChunkStore: Send + Sync {
    fn total_size(&self) -> u64;
    fn chunk_size(&self) -> u64;
    fn read_chunk(&self, chunk_index: u64) -> Result<Option<Vec<u8>>, StreamingDiskError>;
    fn write_chunk(&self, chunk_index: u64, data: &[u8]) -> Result<(), StreamingDiskError>;
    fn clear(&self) -> Result<(), StreamingDiskError>;
    fn flush(&self) -> Result<(), StreamingDiskError>;
}

pub struct SparseFileChunkStore {
    total_size: u64,
    chunk_size: u64,
    file: Mutex<std::fs::File>,
}

impl SparseFileChunkStore {
    pub fn create(
        path: impl AsRef<Path>,
        total_size: u64,
        chunk_size: u64,
    ) -> Result<Self, StreamingDiskError> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| StreamingDiskError::Io(e.to_string()))?;
        file.set_len(total_size)
            .map_err(|e| StreamingDiskError::Io(e.to_string()))?;
        Ok(Self {
            total_size,
            chunk_size,
            file: Mutex::new(file),
        })
    }

    fn chunk_range(&self, chunk_index: u64) -> (u64, u64) {
        let Some(start) = chunk_index.checked_mul(self.chunk_size) else {
            return (self.total_size, self.total_size);
        };
        let end = start.saturating_add(self.chunk_size).min(self.total_size);
        (start, end)
    }
}

impl ChunkStore for SparseFileChunkStore {
    fn total_size(&self) -> u64 {
        self.total_size
    }

    fn chunk_size(&self) -> u64 {
        self.chunk_size
    }

    fn read_chunk(&self, chunk_index: u64) -> Result<Option<Vec<u8>>, StreamingDiskError> {
        let (start, end) = self.chunk_range(chunk_index);
        if start >= end {
            return Ok(Some(Vec::new()));
        }

        let len_u64 = end - start;
        let len: usize = len_u64.try_into().map_err(|_| {
            StreamingDiskError::Protocol(format!("chunk length {len_u64} does not fit in usize"))
        })?;
        let mut buf = alloc_zeroed(len)?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| StreamingDiskError::Io("poisoned lock".to_string()))?;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        Ok(Some(buf))
    }

    fn write_chunk(&self, chunk_index: u64, data: &[u8]) -> Result<(), StreamingDiskError> {
        let (start, end) = self.chunk_range(chunk_index);
        let expected_u64 = end - start;
        let expected: usize = expected_u64.try_into().map_err(|_| {
            StreamingDiskError::Protocol(format!(
                "chunk length {expected_u64} does not fit in usize"
            ))
        })?;
        if data.len() != expected {
            return Err(StreamingDiskError::Protocol(format!(
                "chunk {chunk_index} length mismatch: expected {expected} got {}",
                data.len()
            )));
        }

        let mut file = self
            .file
            .lock()
            .map_err(|_| StreamingDiskError::Io("poisoned lock".to_string()))?;
        file.seek(SeekFrom::Start(start))?;
        file.write_all(data)?;
        Ok(())
    }

    fn clear(&self) -> Result<(), StreamingDiskError> {
        let file = self
            .file
            .lock()
            .map_err(|_| StreamingDiskError::Io("poisoned lock".to_string()))?;
        file.set_len(0)?;
        file.set_len(self.total_size)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), StreamingDiskError> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| StreamingDiskError::Io("poisoned lock".to_string()))?;
        file.flush()?;
        Ok(())
    }
}

pub struct DirectoryChunkStore {
    dir: PathBuf,
    total_size: u64,
    chunk_size: u64,
}

impl DirectoryChunkStore {
    pub fn create(
        dir: impl Into<PathBuf>,
        total_size: u64,
        chunk_size: u64,
    ) -> Result<Self, StreamingDiskError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            total_size,
            chunk_size,
        })
    }

    fn chunk_path(&self, chunk_index: u64) -> PathBuf {
        self.dir.join(format!("{chunk_index}.bin"))
    }

    fn chunk_range(&self, chunk_index: u64) -> (u64, u64) {
        let Some(start) = chunk_index.checked_mul(self.chunk_size) else {
            return (self.total_size, self.total_size);
        };
        let end = start.saturating_add(self.chunk_size).min(self.total_size);
        (start, end)
    }
}

impl ChunkStore for DirectoryChunkStore {
    fn total_size(&self) -> u64 {
        self.total_size
    }

    fn chunk_size(&self) -> u64 {
        self.chunk_size
    }

    fn read_chunk(&self, chunk_index: u64) -> Result<Option<Vec<u8>>, StreamingDiskError> {
        let (start, end) = self.chunk_range(chunk_index);
        if start >= end {
            return Ok(Some(Vec::new()));
        }

        let path = self.chunk_path(chunk_index);
        let expected_u64 = end - start;
        let expected: usize = expected_u64.try_into().map_err(|_| {
            StreamingDiskError::Protocol(format!(
                "chunk length {expected_u64} does not fit in usize"
            ))
        })?;
        let mut file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        match file.metadata() {
            Ok(meta) if meta.len() == expected_u64 => {}
            Ok(_) => {
                // Treat corrupt/mismatched chunks as a cache miss. Best-effort cleanup.
                let _ = fs::remove_file(&path);
                return Ok(None);
            }
            Err(err) => return Err(err.into()),
        }

        let mut bytes = alloc_zeroed(expected)?;
        file.read_exact(&mut bytes)?;
        Ok(Some(bytes))
    }

    fn write_chunk(&self, chunk_index: u64, data: &[u8]) -> Result<(), StreamingDiskError> {
        let (start, end) = self.chunk_range(chunk_index);
        let expected_u64 = end - start;
        let expected: usize = expected_u64.try_into().map_err(|_| {
            StreamingDiskError::Protocol(format!(
                "chunk length {expected_u64} does not fit in usize"
            ))
        })?;
        if data.len() != expected {
            return Err(StreamingDiskError::Protocol(format!(
                "chunk {chunk_index} length mismatch: expected {expected} got {}",
                data.len()
            )));
        }

        let path = self.chunk_path(chunk_index);
        let tmp = path.with_extension("bin.tmp");
        fs::write(&tmp, data)?;
        match fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&path)?;
                fs::rename(&tmp, &path)?;
                Ok(())
            }
            Err(err) => Err(err.into()),
        }
    }

    fn clear(&self) -> Result<(), StreamingDiskError> {
        let _ = fs::remove_dir_all(&self.dir);
        fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), StreamingDiskError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheMeta {
    version: u32,
    total_size: u64,
    validator: Option<String>,
    chunk_size: u64,
    #[serde(default)]
    cache_backend: Option<StreamingCacheBackend>,
    downloaded: RangeSet,
}

impl CacheMeta {
    fn new(
        total_size: u64,
        validator: Option<String>,
        chunk_size: u64,
        cache_backend: StreamingCacheBackend,
    ) -> Self {
        Self {
            version: CACHE_META_VERSION,
            total_size,
            validator,
            chunk_size,
            cache_backend: Some(cache_backend),
            downloaded: RangeSet::new(),
        }
    }
}

fn downloaded_ranges_are_sane(downloaded: &RangeSet, total_size: u64, chunk_size: u64) -> bool {
    // `downloaded` is persisted on disk as JSON and may be corrupted/tampered with. If we trust a
    // bogus `downloaded` set with the sparse-file cache backend, we can end up reading zeros from
    // holes in the sparse file without ever issuing HTTP range requests.
    //
    // Treat any semantic inconsistency as a cache invalidation signal (similar to how malformed
    // JSON is handled in `JsonMetaStore::load`).
    let mut prev_end = 0u64;
    for r in downloaded.ranges() {
        if r.is_empty() {
            return false;
        }
        if r.end > total_size {
            return false;
        }
        if chunk_size == 0 {
            return false;
        }
        // All downloaded ranges are built from `(chunk_index * chunk_size, min(...))`, so starts
        // should always be aligned.
        if r.start % chunk_size != 0 {
            return false;
        }
        // End must be chunk-aligned except for the final chunk, which may end at `total_size`.
        if r.end != total_size && r.end % chunk_size != 0 {
            return false;
        }
        // Ranges must be sorted and non-overlapping.
        if r.start < prev_end {
            return false;
        }
        prev_end = r.end;
    }
    true
}

struct JsonMetaStore {
    path: PathBuf,
}

impl JsonMetaStore {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> Result<Option<CacheMeta>, StreamingDiskError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        match serde_json::from_str(&raw) {
            Ok(meta) => Ok(Some(meta)),
            Err(_) => {
                // Cache metadata is best-effort; treat corruption as an invalidation rather than a
                // fatal error.
                let _ = fs::remove_file(&self.path);
                Ok(None)
            }
        }
    }

    fn save(&self, meta: &CacheMeta) -> Result<(), StreamingDiskError> {
        let raw = serde_json::to_string(meta)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = tmp_path(&self.path);
        fs::write(&tmp, raw)?;
        match fs::rename(&tmp, &self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                match fs::remove_file(&self.path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
                fs::rename(&tmp, &self.path)?;
            }
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    fn remove(&self) -> Result<(), StreamingDiskError> {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

pub struct StreamingDisk {
    inner: Arc<StreamingDiskInner>,
}

struct StreamingDiskInner {
    client: reqwest::Client,
    url: Url,
    request_headers: HeaderMap,
    total_size: u64,
    validator: Option<String>,
    cache_backend: StreamingCacheBackend,
    cache: Arc<dyn ChunkStore>,
    meta_store: JsonMetaStore,
    meta_write_lock: AsyncMutex<()>,
    options: StreamingDiskOptions,
    telemetry: StreamingTelemetry,
    fetch_sem: Semaphore,
    cancel_token: AsyncMutex<CancellationToken>,
    state: AsyncMutex<State>,
}

#[derive(Default)]
struct State {
    downloaded: RangeSet,
    in_flight: HashMap<u64, Vec<oneshot::Sender<Result<(), StreamingDiskError>>>>,
    last_read_end: Option<u64>,
}

impl StreamingDisk {
    pub async fn open(config: StreamingDiskConfig) -> Result<Self, StreamingDiskError> {
        if !config.url.has_host() {
            return Err(StreamingDiskError::UrlNotAbsolute(
                redact_url_for_logs(&config.url).to_string(),
            ));
        }

        if config.options.chunk_size == 0
            || !config
                .options
                .chunk_size
                .is_multiple_of(DEFAULT_SECTOR_SIZE)
        {
            return Err(StreamingDiskError::Protocol(format!(
                "chunk_size must be a non-zero multiple of sector size ({DEFAULT_SECTOR_SIZE})"
            )));
        }
        if config.options.chunk_size > MAX_STREAMING_CHUNK_SIZE {
            return Err(StreamingDiskError::Protocol(format!(
                "chunk_size ({}) exceeds max supported ({MAX_STREAMING_CHUNK_SIZE})",
                config.options.chunk_size
            )));
        }
        if config.options.read_ahead_chunks > MAX_STREAMING_READ_AHEAD_CHUNKS {
            return Err(StreamingDiskError::Protocol(format!(
                "read_ahead_chunks ({}) exceeds max supported ({MAX_STREAMING_READ_AHEAD_CHUNKS})",
                config.options.read_ahead_chunks
            )));
        }
        let read_ahead_bytes = config
            .options
            .read_ahead_chunks
            .checked_mul(config.options.chunk_size)
            .ok_or_else(|| {
                StreamingDiskError::Protocol("read_ahead_chunks * chunk_size overflow".to_string())
            })?;
        if read_ahead_bytes > MAX_STREAMING_READ_AHEAD_BYTES {
            return Err(StreamingDiskError::Protocol(format!(
                "read_ahead prefetch bytes ({read_ahead_bytes}) exceeds max supported ({MAX_STREAMING_READ_AHEAD_BYTES})"
            )));
        }
        if config.options.max_retries == 0 {
            return Err(StreamingDiskError::Protocol(
                "max_retries must be greater than zero".to_string(),
            ));
        }
        if config.options.max_retries > MAX_STREAMING_MAX_RETRIES {
            return Err(StreamingDiskError::Protocol(format!(
                "max_retries ({}) exceeds max supported ({MAX_STREAMING_MAX_RETRIES})",
                config.options.max_retries
            )));
        }
        if config.options.max_concurrent_fetches == 0 {
            return Err(StreamingDiskError::Protocol(
                "max_concurrent_fetches must be greater than zero".to_string(),
            ));
        }
        if config.options.max_concurrent_fetches > MAX_STREAMING_MAX_CONCURRENT_FETCHES {
            return Err(StreamingDiskError::Protocol(format!(
                "max_concurrent_fetches ({}) exceeds max supported ({MAX_STREAMING_MAX_CONCURRENT_FETCHES})",
                config.options.max_concurrent_fetches
            )));
        }

        fs::create_dir_all(&config.cache_dir)?;

        let client = reqwest::Client::new();
        let mut request_headers = build_header_map(&config.request_headers)?;
        // Disk bytes must be served with a stable byte representation. Defensive request to avoid
        // accidental compression at intermediaries.
        request_headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        let (total_size, probed_validator) =
            probe_remote_size_and_validator(&client, &config.url, &request_headers).await?;

        let per_fetch_bytes = config.options.chunk_size.min(total_size);
        let inflight_bytes = (config.options.max_concurrent_fetches as u64)
            .checked_mul(per_fetch_bytes)
            .ok_or_else(|| {
                StreamingDiskError::Protocol(
                    "max_concurrent_fetches * min(chunk_size, total_size) overflow".to_string(),
                )
            })?;
        if inflight_bytes > MAX_STREAMING_INFLIGHT_BYTES {
            return Err(StreamingDiskError::Protocol(format!(
                "inflight download bytes ({inflight_bytes}) exceeds max supported ({MAX_STREAMING_INFLIGHT_BYTES})"
            )));
        }

        if let (Some(expected), Some(actual)) = (&config.validator, &probed_validator) {
            if expected != actual {
                return Err(StreamingDiskError::ValidatorMismatch {
                    expected: Some(expected.clone()),
                    actual: Some(actual.clone()),
                });
            }
        }

        let validator = config.validator.or(probed_validator);

        if let Some(manifest) = &config.options.manifest {
            if manifest.chunk_size != config.options.chunk_size {
                return Err(StreamingDiskError::Protocol(format!(
                    "manifest chunk_size ({}) does not match options.chunk_size ({})",
                    manifest.chunk_size, config.options.chunk_size
                )));
            }

            let expected_chunks = total_size.div_ceil(config.options.chunk_size);
            if manifest.sha256.len() as u64 != expected_chunks {
                return Err(StreamingDiskError::Protocol(format!(
                    "manifest chunk count ({}) does not match expected ({expected_chunks})",
                    manifest.sha256.len()
                )));
            }
        }

        let backend_ok =
            cache_backend_looks_populated(&config.cache_dir, config.cache_backend, total_size);

        let cache: Arc<dyn ChunkStore> = match config.cache_backend {
            StreamingCacheBackend::Directory => Arc::new(DirectoryChunkStore::create(
                config.cache_dir.join(CHUNKS_DIR_NAME),
                total_size,
                config.options.chunk_size,
            )?),
            StreamingCacheBackend::SparseFile => Arc::new(SparseFileChunkStore::create(
                config.cache_dir.join(CACHE_FILE_NAME),
                total_size,
                config.options.chunk_size,
            )?),
        };

        let meta_store = JsonMetaStore::new(config.cache_dir.join(META_FILE_NAME));

        let downloaded = match meta_store.load()? {
            Some(meta)
                if meta.version == CACHE_META_VERSION
                    && meta.total_size == total_size
                    && meta.chunk_size == config.options.chunk_size
                    && meta.validator == validator
                    && meta.cache_backend == Some(config.cache_backend)
                    && downloaded_ranges_are_sane(
                        &meta.downloaded,
                        total_size,
                        config.options.chunk_size,
                    )
                    && backend_ok =>
            {
                meta.downloaded
            }
            Some(_) => {
                // Invalidate: size/validator/chunk size changed. The URL is intentionally
                // *not* part of the cache identity (it may embed ephemeral auth material).
                cache.clear()?;
                meta_store.remove()?;
                let fresh = CacheMeta::new(
                    total_size,
                    validator.clone(),
                    config.options.chunk_size,
                    config.cache_backend,
                );
                meta_store.save(&fresh)?;
                RangeSet::new()
            }
            None => {
                let fresh = CacheMeta::new(
                    total_size,
                    validator.clone(),
                    config.options.chunk_size,
                    config.cache_backend,
                );
                meta_store.save(&fresh)?;
                RangeSet::new()
            }
        };

        Ok(Self {
            inner: Arc::new(StreamingDiskInner {
                client,
                url: config.url,
                request_headers,
                total_size,
                validator,
                cache_backend: config.cache_backend,
                cache,
                meta_store,
                meta_write_lock: AsyncMutex::new(()),
                options: config.options.clone(),
                telemetry: StreamingTelemetry::default(),
                fetch_sem: Semaphore::new(config.options.max_concurrent_fetches.max(1)),
                cancel_token: AsyncMutex::new(CancellationToken::new()),
                state: AsyncMutex::new(State {
                    downloaded,
                    ..State::default()
                }),
            }),
        })
    }

    pub fn total_size(&self) -> u64 {
        self.inner.total_size
    }

    pub fn validator(&self) -> Option<&str> {
        self.inner.validator.as_deref()
    }

    pub fn telemetry_snapshot(&self) -> StreamingTelemetrySnapshot {
        self.inner.telemetry.snapshot()
    }

    pub async fn cache_status(&self) -> CacheStatus {
        let state = self.inner.state.lock().await;
        CacheStatus {
            total_size: self.inner.total_size,
            cached_bytes: state.downloaded.total_len(),
            cached_ranges: state.downloaded.ranges().to_vec(),
            chunk_size: self.inner.options.chunk_size,
            validator: self.inner.validator.clone(),
        }
    }

    pub async fn reset(&self) {
        {
            let mut token = self.inner.cancel_token.lock().await;
            token.cancel();
            *token = CancellationToken::new();
        }

        let mut state = self.inner.state.lock().await;
        let waiters = std::mem::take(&mut state.in_flight);
        state.last_read_end = None;
        drop(state);

        for (_, senders) in waiters {
            for sender in senders {
                let _ = sender.send(Err(StreamingDiskError::Cancelled));
            }
        }
    }

    pub async fn flush(&self) -> Result<(), StreamingDiskError> {
        self.inner.cache.flush()?;
        self.save_meta().await
    }

    /// Read bytes at `offset` into `buf`, fetching any missing chunks via HTTP `Range`.
    pub async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StreamingDiskError> {
        if buf.is_empty() {
            let mut state = self.inner.state.lock().await;
            state.last_read_end = Some(offset);
            return Ok(());
        }

        let len = buf.len() as u64;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| StreamingDiskError::Protocol("read overflow".to_string()))?;
        if end > self.inner.total_size {
            return Err(StreamingDiskError::OutOfBounds {
                offset,
                len,
                size: self.inner.total_size,
            });
        }

        let token = self.inner.cancel_token.lock().await.clone();
        let chunk_size = self.inner.options.chunk_size;

        let (sequential, read_ahead_chunks) = {
            let mut state = self.inner.state.lock().await;
            let sequential = state.last_read_end.is_none_or(|prev| prev == offset);
            state.last_read_end = Some(end);
            (sequential, self.inner.options.read_ahead_chunks)
        };

        let start_chunk = offset / chunk_size;
        let end_chunk = (end.saturating_sub(1)) / chunk_size;

        let mut written = 0usize;
        for chunk_index in start_chunk..=end_chunk {
            self.ensure_chunk_cached(chunk_index, &token).await?;
            let bytes = self.read_chunk_healing(chunk_index).await?;

            let chunk_start = chunk_index * chunk_size;
            let in_chunk_start = if offset > chunk_start {
                (offset - chunk_start) as usize
            } else {
                0
            };

            let max_in_chunk = bytes.len().saturating_sub(in_chunk_start);
            let remaining = buf.len() - written;
            let to_copy = remaining.min(max_in_chunk);
            buf[written..written + to_copy]
                .copy_from_slice(&bytes[in_chunk_start..in_chunk_start + to_copy]);
            written += to_copy;
        }

        if sequential && read_ahead_chunks > 0 {
            let next_chunk = if end == 0 {
                0
            } else {
                (end.saturating_sub(1) / chunk_size) + 1
            };
            self.spawn_prefetch(next_chunk, read_ahead_chunks, token);
        }

        Ok(())
    }

    /// Read `buf.len()` bytes starting at sector `lba`.
    pub async fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), StreamingDiskError> {
        if !(buf.len() as u64).is_multiple_of(DEFAULT_SECTOR_SIZE) {
            return Err(StreamingDiskError::Protocol(format!(
                "read_sectors buffer length must be multiple of {DEFAULT_SECTOR_SIZE}"
            )));
        }
        let offset = lba
            .checked_mul(DEFAULT_SECTOR_SIZE)
            .ok_or_else(|| StreamingDiskError::Protocol("lba overflow".to_string()))?;
        self.read_at(offset, buf).await
    }

    fn spawn_prefetch(&self, start_chunk: u64, count: u64, token: CancellationToken) {
        let disk = self.clone();
        tokio::spawn(async move {
            let chunk_size = disk.inner.options.chunk_size;
            for chunk in start_chunk..start_chunk.saturating_add(count) {
                if token.is_cancelled() {
                    break;
                }
                let Some(chunk_start) = chunk.checked_mul(chunk_size) else {
                    break;
                };
                if chunk_start >= disk.inner.total_size {
                    break;
                }
                let _ = disk.ensure_chunk_cached(chunk, &token).await;
            }
        });
    }

    async fn read_chunk_healing(&self, chunk_index: u64) -> Result<Vec<u8>, StreamingDiskError> {
        match self.inner.cache.read_chunk(chunk_index)? {
            Some(bytes) => Ok(bytes),
            None => {
                // Metadata says the chunk is present but the data is missing/corrupt.
                // Heal by dropping the chunk from the downloaded set and re-fetching.
                let chunk_size = self.inner.options.chunk_size;
                let chunk_start = chunk_index.checked_mul(chunk_size).ok_or_else(|| {
                    StreamingDiskError::Protocol("chunk offset overflow".to_string())
                })?;
                let chunk_end = chunk_start
                    .saturating_add(chunk_size)
                    .min(self.inner.total_size);
                {
                    let mut state = self.inner.state.lock().await;
                    state.downloaded.remove(chunk_start, chunk_end);
                }
                self.save_meta().await?;

                let token = self.inner.cancel_token.lock().await.clone();
                self.ensure_chunk_cached(chunk_index, &token).await?;
                self.inner.cache.read_chunk(chunk_index)?.ok_or_else(|| {
                    StreamingDiskError::Io("chunk vanished after re-download".to_string())
                })
            }
        }
    }

    async fn ensure_chunk_cached(
        &self,
        chunk_index: u64,
        token: &CancellationToken,
    ) -> Result<(), StreamingDiskError> {
        let chunk_size = self.inner.options.chunk_size;
        let Some(chunk_start) = chunk_index.checked_mul(chunk_size) else {
            // Treat overflow as out-of-range; there is nothing to fetch.
            return Ok(());
        };
        if chunk_start >= self.inner.total_size {
            return Ok(());
        }
        let chunk_end = chunk_start
            .saturating_add(chunk_size)
            .min(self.inner.total_size);

        // Serialize in-flight tracking behind a single lock so:
        // - only the first reader performs the HTTP fetch (others join)
        // - telemetry `cache_miss_chunks` counts *downloads*, not joiners.
        let waiter_rx = {
            let mut state = self.inner.state.lock().await;
            if state.downloaded.contains_range(chunk_start, chunk_end) {
                self.inner
                    .telemetry
                    .cache_hit_chunks
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }

            if let Some(waiters) = state.in_flight.get_mut(&chunk_index) {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                Some(rx)
            } else {
                state.in_flight.insert(chunk_index, Vec::new());
                self.inner
                    .telemetry
                    .cache_miss_chunks
                    .fetch_add(1, Ordering::Relaxed);
                None
            }
        };

        if let Some(rx) = waiter_rx {
            return rx
                .await
                .map_err(|_| StreamingDiskError::Cancelled)?
                .map(|_| ());
        }

        let result = self
            .download_and_cache_chunk(chunk_index, chunk_start, chunk_end, token)
            .await;

        let waiters = {
            let mut state = self.inner.state.lock().await;
            state.in_flight.remove(&chunk_index).unwrap_or_default()
        };
        for sender in waiters {
            let _ = sender.send(result.clone());
        }

        result
    }

    async fn download_and_cache_chunk(
        &self,
        chunk_index: u64,
        chunk_start: u64,
        chunk_end: u64,
        token: &CancellationToken,
    ) -> Result<(), StreamingDiskError> {
        let _permit = tokio::select! {
            _ = token.cancelled() => return Err(StreamingDiskError::Cancelled),
            permit = self.inner.fetch_sem.acquire() => permit.map_err(|_| StreamingDiskError::Cancelled)?,
        };

        let bytes = self
            .fetch_with_retries(chunk_start, chunk_end, token)
            .await?;

        if token.is_cancelled() {
            return Err(StreamingDiskError::Cancelled);
        }

        if bytes.len() as u64 != chunk_end - chunk_start {
            return Err(StreamingDiskError::Protocol(format!(
                "short read: expected {} bytes, got {}",
                chunk_end - chunk_start,
                bytes.len()
            )));
        }

        if let Some(manifest) = &self.inner.options.manifest {
            let expected = manifest.sha256_for_chunk(chunk_index).ok_or_else(|| {
                StreamingDiskError::Protocol(format!(
                    "manifest missing sha256 entry for chunk {chunk_index}"
                ))
            })?;
            let actual = Sha256::digest(&bytes);
            let mut actual_arr = [0u8; 32];
            actual_arr.copy_from_slice(&actual);
            if actual_arr != expected {
                return Err(StreamingDiskError::Integrity {
                    chunk_index,
                    expected: hex::encode(expected),
                    actual: hex::encode(actual_arr),
                });
            }
        }

        self.inner.cache.write_chunk(chunk_index, &bytes)?;

        {
            let mut state = self.inner.state.lock().await;
            state.downloaded.insert(chunk_start, chunk_end);
        }
        self.save_meta().await?;
        Ok(())
    }

    async fn save_meta(&self) -> Result<(), StreamingDiskError> {
        // Multiple chunks can be fetched concurrently. Serialize metadata writes so we never
        // race on the `.tmp` file and so the on-disk meta reflects a consistent snapshot.
        let _guard = self.inner.meta_write_lock.lock().await;
        let meta = {
            let state = self.inner.state.lock().await;
            CacheMeta {
                version: CACHE_META_VERSION,
                total_size: self.inner.total_size,
                validator: self.inner.validator.clone(),
                chunk_size: self.inner.options.chunk_size,
                cache_backend: Some(self.inner.cache_backend),
                downloaded: state.downloaded.clone(),
            }
        };
        self.inner.meta_store.save(&meta)?;
        Ok(())
    }

    async fn fetch_with_retries(
        &self,
        start: u64,
        end: u64,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, StreamingDiskError> {
        let mut backoff = Duration::from_millis(100);
        let mut last_err = None;

        for attempt in 0..self.inner.options.max_retries {
            match self.fetch_range_once(start, end, token).await {
                Ok(bytes) => return Ok(bytes),
                Err(e) => {
                    let should_retry = match &e {
                        StreamingDiskError::RangeNotSupported
                        | StreamingDiskError::Integrity { .. }
                        | StreamingDiskError::Protocol(_)
                        | StreamingDiskError::ValidatorMismatch { .. }
                        | StreamingDiskError::Cancelled => false,
                        StreamingDiskError::HttpStatus { status } => {
                            (500..=599).contains(status) || *status == 408 || *status == 429
                        }
                        _ => true,
                    };

                    if !should_retry {
                        return Err(e);
                    }
                    last_err = Some(e);
                    let is_last = attempt + 1 >= self.inner.options.max_retries;
                    if is_last || token.is_cancelled() {
                        break;
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| StreamingDiskError::Http("unknown".to_string())))
    }

    async fn fetch_range_once(
        &self,
        start: u64,
        end: u64,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, StreamingDiskError> {
        if start >= end {
            return Ok(Vec::new());
        }

        let expected_validator = self.inner.validator.as_deref();
        let range_header = format!("bytes={}-{}", start, end - 1);
        self.inner
            .telemetry
            .range_requests
            .fetch_add(1, Ordering::Relaxed);

        let mut headers = self.inner.request_headers.clone();
        headers.insert(
            RANGE,
            HeaderValue::from_str(&range_header)
                .map_err(|e| StreamingDiskError::Protocol(e.to_string()))?,
        );
        // RFC 9110 disallows weak validators in `If-Range` and requires strong comparison. Some
        // servers respond with `200 OK` (full representation) when clients send `If-Range: W/"..."`,
        // which is ambiguous with servers that don't support Range at all. Avoid the ambiguity by
        // omitting `If-Range` when the validator is a weak ETag and instead validating ETags on
        // successful `206` responses.
        let sent_if_range = if let Some(validator) = expected_validator {
            if !validator_is_weak_etag(validator) {
                headers.insert(
                    IF_RANGE,
                    HeaderValue::from_str(validator)
                        .map_err(|e| StreamingDiskError::Protocol(e.to_string()))?,
                );
                true
            } else {
                false
            }
        } else {
            false
        };
        let req = self
            .inner
            .client
            .get(self.inner.url.clone())
            .headers(headers);

        let resp = tokio::select! {
            _ = token.cancelled() => return Err(StreamingDiskError::Cancelled),
            resp = req.send() => resp.map_err(|e| StreamingDiskError::Http(format_reqwest_error(e)))?,
        };

        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            if let Some(expected) = &self.inner.validator {
                // Per RFC 7233, a server will return the full representation (200) when an
                // `If-Range` validator does not match. Some implementations use `412
                // Precondition Failed` instead.
                //
                // However, a server that does *not* support Range may also reply with 200. To
                // avoid mislabeling the error, only treat 200 as a validator mismatch when the
                // server provides a validator that differs from the requested validator.
                if resp.status() == reqwest::StatusCode::PRECONDITION_FAILED {
                    let actual = extract_validator(resp.headers());
                    return Err(StreamingDiskError::ValidatorMismatch {
                        expected: Some(expected.clone()),
                        actual,
                    });
                }
                if resp.status() == reqwest::StatusCode::OK {
                    let actual = extract_validator(resp.headers());
                    if actual
                        .as_deref()
                        .is_some_and(|etag| etag != expected.as_str())
                    {
                        return Err(StreamingDiskError::ValidatorMismatch {
                            expected: Some(expected.clone()),
                            actual,
                        });
                    }
                }
            }
            if resp.status().is_success() {
                return Err(StreamingDiskError::RangeNotSupported);
            }
            return Err(StreamingDiskError::HttpStatus {
                status: resp.status().as_u16(),
            });
        }

        let label = format!("range response bytes={start}-{}", end - 1);
        require_no_transform_cache_control(resp.headers(), &label)?;

        if !sent_if_range {
            if let Some(expected) = expected_validator {
                if validator_is_weak_etag(expected) {
                    if let Some(actual) = resp.headers().get(ETAG).and_then(|v| v.to_str().ok()) {
                        if actual != expected {
                            return Err(StreamingDiskError::ValidatorMismatch {
                                expected: Some(expected.to_string()),
                                actual: Some(actual.to_string()),
                            });
                        }
                    }
                }
            }
        }

        if let Some(encoding) = resp
            .headers()
            .get(CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
        {
            let encoding = encoding.trim();
            if !encoding.eq_ignore_ascii_case("identity") {
                return Err(StreamingDiskError::Protocol(format!(
                    "unexpected Content-Encoding: {encoding}"
                )));
            }
        }

        let content_range = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| StreamingDiskError::Protocol("missing Content-Range".to_string()))?;
        let (cr_start, cr_end_inclusive, cr_total) = parse_content_range(content_range)?;
        if cr_start != start
            || cr_end_inclusive != end - 1
            || cr_total.is_some_and(|total| total != self.inner.total_size)
        {
            return Err(StreamingDiskError::Protocol(format!(
                "unexpected Content-Range: {content_range} (expected bytes {start}-{} / {})",
                end - 1,
                self.inner.total_size
            )));
        }

        let bytes = tokio::select! {
            _ = token.cancelled() => return Err(StreamingDiskError::Cancelled),
            bytes = resp.bytes() => bytes.map_err(|e| StreamingDiskError::Http(format_reqwest_error(e)))?,
        };

        self.inner
            .telemetry
            .bytes_downloaded
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);

        Ok(bytes.to_vec())
    }
}

impl Clone for StreamingDisk {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

async fn probe_remote_size_and_validator(
    client: &reqwest::Client,
    url: &Url,
    request_headers: &HeaderMap,
) -> Result<(u64, Option<String>), StreamingDiskError> {
    let mut head_total_size: Option<u64> = None;
    let mut head_validator: Option<String> = None;

    let head = client
        .head(url.clone())
        .headers(request_headers.clone())
        .send()
        .await;
    if let Ok(resp) = head {
        if resp.status().is_success() {
            head_total_size = resp
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            head_validator = extract_validator(resp.headers());

            let accept_ranges = resp
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_ascii_lowercase());

            if accept_ranges.as_deref() == Some("bytes") {
                if let Some(len) = head_total_size {
                    return Ok((len, head_validator));
                }
            }
        }
    }

    let resp = client
        .get(url.clone())
        .headers(request_headers.clone())
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| StreamingDiskError::Http(format_reqwest_error(e)))?;

    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        if resp.status().is_success() {
            return Err(StreamingDiskError::RangeNotSupported);
        }
        return Err(StreamingDiskError::HttpStatus {
            status: resp.status().as_u16(),
        });
    }

    require_no_transform_cache_control(resp.headers(), "range probe")?;

    let validator = extract_validator(resp.headers());

    let cr = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| StreamingDiskError::Protocol("missing Content-Range".to_string()))?;

    let total = match (parse_total_size_from_content_range(cr)?, head_total_size) {
        (Some(total), Some(expected)) => {
            if total != expected {
                return Err(StreamingDiskError::Protocol(format!(
                    "Content-Range total ({total}) does not match Content-Length ({expected})"
                )));
            }
            total
        }
        (Some(total), None) => total,
        (None, Some(expected)) => expected,
        (None, None) => {
            return Err(StreamingDiskError::Protocol(
                "Content-Range did not include total size and Content-Length was missing"
                    .to_string(),
            ));
        }
    };
    Ok((total, validator.or(head_validator)))
}

fn build_header_map(headers: &[(String, String)]) -> Result<HeaderMap, StreamingDiskError> {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let name_lower = name.to_ascii_lowercase();
        let name = HeaderName::from_bytes(name_lower.as_bytes())
            .map_err(|e| StreamingDiskError::Protocol(e.to_string()))?;
        let value = HeaderValue::from_str(value)
            .map_err(|e| StreamingDiskError::Protocol(e.to_string()))?;
        out.insert(name, value);
    }
    Ok(out)
}

fn redact_url_for_logs(url: &Url) -> Url {
    let mut url = url.clone();
    // Username/password are rarely used, but if present they are sensitive.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn format_reqwest_error(err: reqwest::Error) -> String {
    let mut msg = err.to_string();
    if let Some(url) = err.url() {
        let redacted = redact_url_for_logs(url);
        msg = msg.replace(url.as_str(), redacted.as_str());
    }
    msg
}

fn extract_validator(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string())
        .or_else(|| {
            headers
                .get(LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string())
        })
}

fn validator_is_weak_etag(validator: &str) -> bool {
    let trimmed = validator.trim_start();
    trimmed.starts_with("W/") || trimmed.starts_with("w/")
}

fn cache_backend_looks_populated(
    cache_dir: &Path,
    backend: StreamingCacheBackend,
    total_size: u64,
) -> bool {
    match backend {
        StreamingCacheBackend::Directory => cache_dir.join(CHUNKS_DIR_NAME).is_dir(),
        StreamingCacheBackend::SparseFile => {
            let path = cache_dir.join(CACHE_FILE_NAME);
            match fs::metadata(path) {
                Ok(meta) => meta.len() == total_size,
                Err(_) => false,
            }
        }
    }
}

fn parse_total_size_from_content_range(
    content_range: &str,
) -> Result<Option<u64>, StreamingDiskError> {
    let (_, _, total) = parse_content_range(content_range)?;
    Ok(total)
}

fn parse_content_range(content_range: &str) -> Result<(u64, u64, Option<u64>), StreamingDiskError> {
    // Example: "bytes 0-0/12345"
    let content_range = content_range.trim();
    let mut parts = content_range.split_whitespace();
    let unit = parts.next().ok_or_else(|| {
        StreamingDiskError::Protocol(format!("invalid Content-Range: {content_range}"))
    })?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(StreamingDiskError::Protocol(format!(
            "invalid Content-Range unit: {content_range}"
        )));
    }
    let spec = parts.next().ok_or_else(|| {
        StreamingDiskError::Protocol(format!("invalid Content-Range: {content_range}"))
    })?;

    let (range_part, total_part) = spec.split_once('/').ok_or_else(|| {
        StreamingDiskError::Protocol(format!("invalid Content-Range: {content_range}"))
    })?;
    let total = if total_part == "*" {
        None
    } else {
        Some(total_part.parse().map_err(|_| {
            StreamingDiskError::Protocol(format!("invalid Content-Range: {content_range}"))
        })?)
    };

    let (start_part, end_part) = range_part.split_once('-').ok_or_else(|| {
        StreamingDiskError::Protocol(format!("invalid Content-Range: {content_range}"))
    })?;
    let start: u64 = start_part.parse().map_err(|_| {
        StreamingDiskError::Protocol(format!("invalid Content-Range: {content_range}"))
    })?;
    let end: u64 = end_part.parse().map_err(|_| {
        StreamingDiskError::Protocol(format!("invalid Content-Range: {content_range}"))
    })?;
    if end < start {
        return Err(StreamingDiskError::Protocol(format!(
            "invalid Content-Range: {content_range}"
        )));
    }
    if let Some(total) = total {
        if end >= total {
            return Err(StreamingDiskError::Protocol(format!(
                "invalid Content-Range: {content_range}"
            )));
        }
    }
    Ok((start, end, total))
}

// We use `hex` only for integrity error messages. Keep it private to avoid committing
// to a public dependency in the API surface.
mod hex {
    pub fn encode(bytes: [u8; 32]) -> String {
        const LUT: &[u8; 16] = b"0123456789abcdef";
        let mut out = [0u8; 64];
        for (i, b) in bytes.iter().copied().enumerate() {
            out[i * 2] = LUT[(b >> 4) as usize];
            out[i * 2 + 1] = LUT[(b & 0xF) as usize];
        }
        // Safety: LUT is valid UTF-8.
        unsafe { String::from_utf8_unchecked(out.to_vec()) }
    }
}
