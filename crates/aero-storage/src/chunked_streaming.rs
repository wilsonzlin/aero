//! Native-only helper for streaming chunked disk images (`manifest.json` + `chunks/*.bin`).
//!
//! This is the Rust/native counterpart to `web/src/storage/remote_chunked_disk.ts` and implements
//! the format specified in `docs/18-chunked-disk-image-format.md`.
//!
//! Unlike [`crate::StreamingDisk`], this implementation does **not** rely on HTTP `Range`
//! requests. Each chunk is fetched with a plain `GET` to a stable per-chunk URL.
#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::range_set::RangeSet;
use crate::streaming::{
    require_no_transform_cache_control, ChunkStore, DirectoryChunkStore, SparseFileChunkStore,
    StreamingCacheBackend, StreamingDiskError,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{oneshot, Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;
use url::Url;

const MANIFEST_SCHEMA_V1: &str = "aero.chunked-disk-image.v1";

const META_FILE_NAME: &str = "chunked-streaming-cache-meta.json";
const CHUNKS_DIR_NAME: &str = "chunked-chunks";
const CACHE_FILE_NAME: &str = "chunked-cache.bin";

const CACHE_META_VERSION: u32 = 1;

const SECTOR_SIZE_BYTES: u64 = crate::SECTOR_SIZE as u64;

// Keep these bounds aligned with `web/src/storage/remote_chunked_disk.ts` where sensible.
// 64 MiB.
const MAX_CHUNK_SIZE_BYTES: u64 = 64 * 1024 * 1024;
// 500k chunks.
const MAX_CHUNK_COUNT: u64 = 500_000;
// 32 chars of zero padding is more than enough for any practical image size, while still bounding
// attacker-controlled allocations when deriving chunk URLs from untrusted manifests.
const MAX_CHUNK_INDEX_WIDTH: usize = 32;
// 64 MiB.
const MAX_MANIFEST_JSON_BYTES: usize = 64 * 1024 * 1024;

const MAX_MAX_RETRIES: usize = 32;
const MAX_MAX_CONCURRENT_FETCHES: usize = 128;
// 512 MiB.
const MAX_INFLIGHT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error, Clone)]
pub enum ChunkedStreamingDiskError {
    #[error("unsupported manifest schema: {0}")]
    UnsupportedManifestSchema(String),

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

    #[error("operation cancelled")]
    Cancelled,

    #[error("out of bounds access: offset {offset} len {len} size {size}")]
    OutOfBounds { offset: u64, len: u64, size: u64 },

    #[error("URL must be absolute: {0}")]
    UrlNotAbsolute(String),
}

impl From<std::io::Error> for ChunkedStreamingDiskError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for ChunkedStreamingDiskError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value.to_string())
    }
}

impl From<StreamingDiskError> for ChunkedStreamingDiskError {
    fn from(value: StreamingDiskError) -> Self {
        match value {
            StreamingDiskError::RangeNotSupported => ChunkedStreamingDiskError::Protocol(
                "remote server does not support HTTP Range requests".to_string(),
            ),
            StreamingDiskError::HttpStatus { status } => {
                ChunkedStreamingDiskError::HttpStatus { status }
            }
            StreamingDiskError::Http(msg) => ChunkedStreamingDiskError::Http(msg),
            StreamingDiskError::Protocol(msg) => ChunkedStreamingDiskError::Protocol(msg),
            StreamingDiskError::Io(msg) => ChunkedStreamingDiskError::Io(msg),
            StreamingDiskError::Serde(msg) => ChunkedStreamingDiskError::Serde(msg),
            StreamingDiskError::Integrity {
                chunk_index,
                expected,
                actual,
            } => ChunkedStreamingDiskError::Integrity {
                chunk_index,
                expected,
                actual,
            },
            StreamingDiskError::ValidatorMismatch { expected, actual } => {
                ChunkedStreamingDiskError::Protocol(format!(
                    "remote validator mismatch (expected {expected:?}, got {actual:?})"
                ))
            }
            StreamingDiskError::Cancelled => ChunkedStreamingDiskError::Cancelled,
            StreamingDiskError::OutOfBounds { offset, len, size } => {
                ChunkedStreamingDiskError::OutOfBounds { offset, len, size }
            }
            StreamingDiskError::UrlNotAbsolute(s) => ChunkedStreamingDiskError::UrlNotAbsolute(s),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkedStreamingDiskOptions {
    /// Maximum concurrent HTTP `GET` chunk fetches.
    pub max_concurrent_fetches: usize,
    /// Maximum number of attempts for a single chunk fetch (includes the first attempt).
    pub max_retries: usize,
}

impl Default for ChunkedStreamingDiskOptions {
    fn default() -> Self {
        Self {
            max_concurrent_fetches: 4,
            max_retries: 4,
        }
    }
}

#[derive(Clone)]
pub struct ChunkedStreamingDiskConfig {
    pub manifest_url: Url,
    pub cache_dir: PathBuf,
    /// Additional headers applied to all HTTP requests (`GET manifest.json` + `GET chunk`).
    ///
    /// This is intended for auth (`Authorization`, `Cookie`, etc). These headers are *not*
    /// persisted in the cache identity.
    pub request_headers: Vec<(String, String)>,
    pub cache_backend: StreamingCacheBackend,
    pub options: ChunkedStreamingDiskOptions,
}

impl fmt::Debug for ChunkedStreamingDiskConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The URL and request headers may embed auth material (signed URLs, Authorization tokens).
        // Redact by default to avoid accidental leakage in logs.
        let url = redact_url_for_logs(&self.manifest_url);
        let header_names: Vec<&str> = self
            .request_headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();

        f.debug_struct("ChunkedStreamingDiskConfig")
            .field("manifest_url", &url)
            .field("cache_dir", &self.cache_dir)
            .field("request_headers", &header_names)
            .field("cache_backend", &self.cache_backend)
            .field("options", &self.options)
            .finish()
    }
}

impl ChunkedStreamingDiskConfig {
    pub fn new(manifest_url: Url, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            manifest_url,
            cache_dir: cache_dir.into(),
            request_headers: Vec::new(),
            cache_backend: StreamingCacheBackend::default(),
            options: ChunkedStreamingDiskOptions::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkedDiskManifestV1 {
    pub version: String,
    pub mime_type: String,
    pub total_size: u64,
    pub chunk_size: u64,
    pub chunk_count: u64,
    pub chunk_index_width: usize,
    pub chunk_sha256: Vec<Option<[u8; 32]>>,
}

impl ChunkedDiskManifestV1 {
    pub fn sha256_for_chunk(&self, chunk_index: u64) -> Option<[u8; 32]> {
        let idx: usize = chunk_index.try_into().ok()?;
        self.chunk_sha256.get(idx).copied().flatten()
    }

    pub fn chunk_len(&self, chunk_index: u64) -> u64 {
        let Some(start) = chunk_index.checked_mul(self.chunk_size) else {
            return 0;
        };
        if start >= self.total_size {
            return 0;
        }
        let end = start.saturating_add(self.chunk_size).min(self.total_size);
        end - start
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestV1Raw {
    schema: String,
    version: String,
    mime_type: String,
    total_size: u64,
    chunk_size: u64,
    chunk_count: u64,
    chunk_index_width: u64,
    #[serde(default)]
    chunks: Option<Vec<ChunkEntryRaw>>,
}

#[derive(Debug, Deserialize)]
struct ChunkEntryRaw {
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    sha256: Option<String>,
}

fn parse_hex_sha256(value: &str) -> Result<[u8; 32], ChunkedStreamingDiskError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != 64 {
        return Err(ChunkedStreamingDiskError::Protocol(
            "sha256 must be a 64-char hex string".to_string(),
        ));
    }
    let mut out = [0u8; 32];
    let bytes = normalized.as_bytes();
    let hex_val = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        }
    };
    for (i, out_byte) in out.iter_mut().enumerate() {
        let hi = bytes[i * 2];
        let lo = bytes[i * 2 + 1];
        let Some(hi) = hex_val(hi) else {
            return Err(ChunkedStreamingDiskError::Protocol(
                "sha256 must be a 64-char hex string".to_string(),
            ));
        };
        let Some(lo) = hex_val(lo) else {
            return Err(ChunkedStreamingDiskError::Protocol(
                "sha256 must be a 64-char hex string".to_string(),
            ));
        };
        *out_byte = (hi << 4) | lo;
    }
    Ok(out)
}

fn parse_manifest_v1(
    raw: ManifestV1Raw,
) -> Result<ChunkedDiskManifestV1, ChunkedStreamingDiskError> {
    if raw.schema != MANIFEST_SCHEMA_V1 {
        return Err(ChunkedStreamingDiskError::UnsupportedManifestSchema(
            raw.schema,
        ));
    }

    if raw.version.trim().is_empty() {
        return Err(ChunkedStreamingDiskError::Protocol(
            "manifest version must be a non-empty string".to_string(),
        ));
    }
    if raw.mime_type.trim().is_empty() {
        return Err(ChunkedStreamingDiskError::Protocol(
            "manifest mimeType must be a non-empty string".to_string(),
        ));
    }

    if raw.total_size == 0 {
        return Err(ChunkedStreamingDiskError::Protocol(
            "totalSize must be > 0".to_string(),
        ));
    }
    if !raw.total_size.is_multiple_of(SECTOR_SIZE_BYTES) {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "totalSize must be a multiple of {SECTOR_SIZE_BYTES}"
        )));
    }

    if raw.chunk_size == 0 {
        return Err(ChunkedStreamingDiskError::Protocol(
            "chunkSize must be > 0".to_string(),
        ));
    }
    if !raw.chunk_size.is_multiple_of(SECTOR_SIZE_BYTES) {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "chunkSize must be a multiple of {SECTOR_SIZE_BYTES}"
        )));
    }
    if raw.chunk_size > MAX_CHUNK_SIZE_BYTES {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "chunkSize ({}) exceeds max supported ({MAX_CHUNK_SIZE_BYTES})",
            raw.chunk_size
        )));
    }

    if raw.chunk_count == 0 {
        return Err(ChunkedStreamingDiskError::Protocol(
            "chunkCount must be > 0".to_string(),
        ));
    }
    if raw.chunk_count > MAX_CHUNK_COUNT {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "chunkCount ({}) exceeds max supported ({MAX_CHUNK_COUNT})",
            raw.chunk_count
        )));
    }
    let chunk_count_usize: usize = raw.chunk_count.try_into().map_err(|_| {
        ChunkedStreamingDiskError::Protocol("chunkCount does not fit in usize".to_string())
    })?;

    if raw.chunk_index_width == 0 {
        return Err(ChunkedStreamingDiskError::Protocol(
            "chunkIndexWidth must be > 0".to_string(),
        ));
    }
    let chunk_index_width: usize = raw.chunk_index_width.try_into().map_err(|_| {
        ChunkedStreamingDiskError::Protocol("chunkIndexWidth does not fit in usize".to_string())
    })?;
    if chunk_index_width > MAX_CHUNK_INDEX_WIDTH {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "chunkIndexWidth too large: max={MAX_CHUNK_INDEX_WIDTH} got={chunk_index_width}"
        )));
    }

    let expected_count = raw.total_size.div_ceil(raw.chunk_size);
    if raw.chunk_count != expected_count {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "chunkCount mismatch: expected={expected_count} manifest={}",
            raw.chunk_count
        )));
    }

    let min_width = raw.chunk_count.saturating_sub(1).to_string().len().max(1);
    if chunk_index_width < min_width {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "chunkIndexWidth too small: need>={min_width} got={chunk_index_width}"
        )));
    }

    let derived_last_size = raw
        .chunk_size
        .checked_mul(raw.chunk_count.saturating_sub(1))
        .and_then(|v| raw.total_size.checked_sub(v))
        .ok_or_else(|| {
            ChunkedStreamingDiskError::Protocol("invalid derived final chunk size".to_string())
        })?;
    if derived_last_size == 0 || derived_last_size > raw.chunk_size {
        return Err(ChunkedStreamingDiskError::Protocol(
            "invalid derived final chunk size".to_string(),
        ));
    }

    let mut chunk_sha256: Vec<Option<[u8; 32]>> = vec![None; chunk_count_usize];

    if let Some(chunks) = raw.chunks {
        if chunks.len() != chunk_count_usize {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "chunks.length mismatch: expected={} actual={}",
                chunk_count_usize,
                chunks.len()
            )));
        }

        for (i, item) in chunks.iter().enumerate() {
            let expected_size = if i + 1 == chunk_count_usize {
                derived_last_size
            } else {
                raw.chunk_size
            };
            let actual_size = item.size.unwrap_or(expected_size);
            if actual_size != expected_size {
                return Err(ChunkedStreamingDiskError::Protocol(format!(
                    "chunks[{i}].size mismatch: expected={expected_size} actual={actual_size}"
                )));
            }

            if let Some(sha) = item.sha256.as_deref() {
                chunk_sha256[i] = Some(parse_hex_sha256(sha).map_err(|_| {
                    ChunkedStreamingDiskError::Protocol(format!(
                        "chunks[{i}].sha256 must be a 64-char hex string"
                    ))
                })?);
            }
        }
    }

    // Validate the derived chunk sizes sum to totalSize.
    let sum = raw
        .chunk_count
        .saturating_sub(1)
        .checked_mul(raw.chunk_size)
        .and_then(|v| v.checked_add(derived_last_size))
        .ok_or_else(|| {
            ChunkedStreamingDiskError::Protocol("chunk size sum overflow".to_string())
        })?;
    if sum != raw.total_size {
        return Err(ChunkedStreamingDiskError::Protocol(format!(
            "chunk sizes do not sum to totalSize: sum={sum} totalSize={}",
            raw.total_size
        )));
    }

    Ok(ChunkedDiskManifestV1 {
        version: raw.version,
        mime_type: raw.mime_type,
        total_size: raw.total_size,
        chunk_size: raw.chunk_size,
        chunk_count: raw.chunk_count,
        chunk_index_width,
        chunk_sha256,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheMeta {
    version: u32,
    schema: String,
    manifest_version: String,
    manifest_sha256: String,
    total_size: u64,
    chunk_size: u64,
    chunk_count: u64,
    chunk_index_width: u64,
    #[serde(default)]
    cache_backend: Option<StreamingCacheBackend>,
    downloaded: RangeSet,
}

impl CacheMeta {
    fn fresh(
        manifest: &ChunkedDiskManifestV1,
        manifest_sha256: String,
        cache_backend: StreamingCacheBackend,
    ) -> Self {
        Self {
            version: CACHE_META_VERSION,
            schema: MANIFEST_SCHEMA_V1.to_string(),
            manifest_version: manifest.version.clone(),
            manifest_sha256,
            total_size: manifest.total_size,
            chunk_size: manifest.chunk_size,
            chunk_count: manifest.chunk_count,
            chunk_index_width: manifest.chunk_index_width as u64,
            cache_backend: Some(cache_backend),
            downloaded: RangeSet::new(),
        }
    }
}

struct JsonMetaStore {
    path: PathBuf,
}

impl JsonMetaStore {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> Result<Option<CacheMeta>, ChunkedStreamingDiskError> {
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

    fn save(&self, meta: &CacheMeta) -> Result<(), ChunkedStreamingDiskError> {
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

    fn remove(&self) -> Result<(), ChunkedStreamingDiskError> {
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

#[derive(Default)]
struct Telemetry {
    pub bytes_downloaded: AtomicU64,
    pub http_gets: AtomicU64,
    pub cache_hit_chunks: AtomicU64,
    pub cache_miss_chunks: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedStreamingTelemetrySnapshot {
    pub bytes_downloaded: u64,
    pub http_gets: u64,
    pub cache_hit_chunks: u64,
    pub cache_miss_chunks: u64,
}

impl Telemetry {
    fn snapshot(&self) -> ChunkedStreamingTelemetrySnapshot {
        ChunkedStreamingTelemetrySnapshot {
            bytes_downloaded: self.bytes_downloaded.load(Ordering::Relaxed),
            http_gets: self.http_gets.load(Ordering::Relaxed),
            cache_hit_chunks: self.cache_hit_chunks.load(Ordering::Relaxed),
            cache_miss_chunks: self.cache_miss_chunks.load(Ordering::Relaxed),
        }
    }
}

pub struct ChunkedStreamingDisk {
    inner: Arc<ChunkedStreamingDiskInner>,
}

struct ChunkedStreamingDiskInner {
    client: reqwest::Client,
    manifest_url: Url,
    request_headers: HeaderMap,
    manifest: ChunkedDiskManifestV1,
    manifest_sha256: [u8; 32],
    cache_backend: StreamingCacheBackend,
    cache: Arc<dyn ChunkStore>,
    meta_store: JsonMetaStore,
    meta_write_lock: AsyncMutex<()>,
    options: ChunkedStreamingDiskOptions,
    telemetry: Telemetry,
    fetch_sem: Semaphore,
    cancel_token: AsyncMutex<CancellationToken>,
    state: AsyncMutex<State>,
}

#[derive(Default)]
struct State {
    downloaded: RangeSet,
    in_flight: HashMap<u64, Vec<oneshot::Sender<Result<(), ChunkedStreamingDiskError>>>>,
    last_read_end: Option<u64>,
}

impl Clone for ChunkedStreamingDisk {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl ChunkedStreamingDisk {
    pub async fn open(
        config: ChunkedStreamingDiskConfig,
    ) -> Result<Self, ChunkedStreamingDiskError> {
        if !config.manifest_url.has_host() {
            return Err(ChunkedStreamingDiskError::UrlNotAbsolute(
                redact_url_for_logs(&config.manifest_url).to_string(),
            ));
        }

        if config.options.max_retries == 0 {
            return Err(ChunkedStreamingDiskError::Protocol(
                "max_retries must be greater than zero".to_string(),
            ));
        }
        if config.options.max_retries > MAX_MAX_RETRIES {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "max_retries ({}) exceeds max supported ({MAX_MAX_RETRIES})",
                config.options.max_retries
            )));
        }
        if config.options.max_concurrent_fetches == 0 {
            return Err(ChunkedStreamingDiskError::Protocol(
                "max_concurrent_fetches must be greater than zero".to_string(),
            ));
        }
        if config.options.max_concurrent_fetches > MAX_MAX_CONCURRENT_FETCHES {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "max_concurrent_fetches ({}) exceeds max supported ({MAX_MAX_CONCURRENT_FETCHES})",
                config.options.max_concurrent_fetches
            )));
        }

        fs::create_dir_all(&config.cache_dir)?;

        let client = reqwest::Client::new();
        let mut request_headers = build_header_map(&config.request_headers)?;
        // Defensive request: disk bytes should not be compressed/transformed.
        request_headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));

        let (manifest, manifest_sha256) =
            fetch_and_parse_manifest(&client, &config.manifest_url, &request_headers).await?;

        let per_fetch_bytes = manifest.chunk_size.min(manifest.total_size);
        let inflight_bytes = (config.options.max_concurrent_fetches as u64)
            .checked_mul(per_fetch_bytes)
            .ok_or_else(|| {
                ChunkedStreamingDiskError::Protocol(
                    "max_concurrent_fetches * min(chunkSize, totalSize) overflow".to_string(),
                )
            })?;
        if inflight_bytes > MAX_INFLIGHT_BYTES {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "inflight download bytes ({inflight_bytes}) exceeds max supported ({MAX_INFLIGHT_BYTES})"
            )));
        }

        let backend_ok = cache_backend_looks_populated(
            &config.cache_dir,
            config.cache_backend,
            manifest.total_size,
        );

        let cache: Arc<dyn ChunkStore> = match config.cache_backend {
            StreamingCacheBackend::Directory => Arc::new(DirectoryChunkStore::create(
                config.cache_dir.join(CHUNKS_DIR_NAME),
                manifest.total_size,
                manifest.chunk_size,
            )?),
            StreamingCacheBackend::SparseFile => Arc::new(SparseFileChunkStore::create(
                config.cache_dir.join(CACHE_FILE_NAME),
                manifest.total_size,
                manifest.chunk_size,
            )?),
        };

        let meta_store = JsonMetaStore::new(config.cache_dir.join(META_FILE_NAME));
        let manifest_sha256_hex = hex::encode(manifest_sha256);

        let downloaded = match meta_store.load()? {
            Some(meta)
                if meta.version == CACHE_META_VERSION
                    && meta.schema == MANIFEST_SCHEMA_V1
                    && meta.manifest_version == manifest.version
                    && meta.manifest_sha256 == manifest_sha256_hex
                    && meta.total_size == manifest.total_size
                    && meta.chunk_size == manifest.chunk_size
                    && meta.chunk_count == manifest.chunk_count
                    && meta.chunk_index_width == manifest.chunk_index_width as u64
                    && meta.cache_backend == Some(config.cache_backend)
                    && backend_ok =>
            {
                meta.downloaded
            }
            Some(_) => {
                cache.clear()?;
                meta_store.remove()?;
                let fresh =
                    CacheMeta::fresh(&manifest, manifest_sha256_hex.clone(), config.cache_backend);
                meta_store.save(&fresh)?;
                RangeSet::new()
            }
            None => {
                let fresh =
                    CacheMeta::fresh(&manifest, manifest_sha256_hex.clone(), config.cache_backend);
                meta_store.save(&fresh)?;
                RangeSet::new()
            }
        };

        Ok(Self {
            inner: Arc::new(ChunkedStreamingDiskInner {
                client,
                manifest_url: config.manifest_url,
                request_headers,
                manifest,
                manifest_sha256,
                cache_backend: config.cache_backend,
                cache,
                meta_store,
                meta_write_lock: AsyncMutex::new(()),
                options: config.options.clone(),
                telemetry: Telemetry::default(),
                fetch_sem: Semaphore::new(config.options.max_concurrent_fetches.max(1)),
                cancel_token: AsyncMutex::new(CancellationToken::new()),
                state: AsyncMutex::new(State {
                    downloaded,
                    ..State::default()
                }),
            }),
        })
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.inner.manifest.total_size
    }

    pub fn manifest(&self) -> &ChunkedDiskManifestV1 {
        &self.inner.manifest
    }

    pub fn manifest_sha256(&self) -> [u8; 32] {
        self.inner.manifest_sha256
    }

    pub fn telemetry_snapshot(&self) -> ChunkedStreamingTelemetrySnapshot {
        self.inner.telemetry.snapshot()
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
                let _ = sender.send(Err(ChunkedStreamingDiskError::Cancelled));
            }
        }
    }

    pub async fn flush(&self) -> Result<(), ChunkedStreamingDiskError> {
        self.inner.cache.flush()?;
        self.save_meta().await
    }

    /// Read bytes at `offset` into `buf`, fetching any missing chunks via plain HTTP `GET`.
    pub async fn read_at(
        &self,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), ChunkedStreamingDiskError> {
        if buf.is_empty() {
            let mut state = self.inner.state.lock().await;
            state.last_read_end = Some(offset);
            return Ok(());
        }

        let len = buf.len() as u64;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| ChunkedStreamingDiskError::Protocol("read overflow".to_string()))?;
        if end > self.inner.manifest.total_size {
            return Err(ChunkedStreamingDiskError::OutOfBounds {
                offset,
                len,
                size: self.inner.manifest.total_size,
            });
        }

        // Update sequential tracking even though we currently do not implement read-ahead. This
        // keeps parity with `StreamingDisk` and allows future optimizations.
        {
            let mut state = self.inner.state.lock().await;
            state.last_read_end = Some(end);
        }

        let token = self.inner.cancel_token.lock().await.clone();
        let chunk_size = self.inner.manifest.chunk_size;
        let start_chunk = offset / chunk_size;
        let end_chunk = (end.saturating_sub(1)) / chunk_size;

        let max_inflight = self.inner.options.max_concurrent_fetches.max(1);
        let mut next_chunk = start_chunk;
        let mut join_set: tokio::task::JoinSet<Result<u64, ChunkedStreamingDiskError>> =
            tokio::task::JoinSet::new();

        let launch = |chunk_index: u64,
                      join_set: &mut tokio::task::JoinSet<
            Result<u64, ChunkedStreamingDiskError>,
        >| {
            let disk = self.clone();
            let token = token.clone();
            join_set.spawn(async move {
                disk.ensure_chunk_cached(chunk_index, &token).await?;
                Ok(chunk_index)
            });
        };

        while next_chunk <= end_chunk && join_set.len() < max_inflight {
            launch(next_chunk, &mut join_set);
            next_chunk += 1;
        }

        while let Some(res) = join_set.join_next().await {
            let chunk_index = match res {
                Ok(v) => v?,
                Err(err) => {
                    return Err(ChunkedStreamingDiskError::Io(format!("join error: {err}")))
                }
            };

            let bytes = self.read_chunk_healing(chunk_index).await?;
            copy_from_chunk(chunk_index, chunk_size, offset, end, &bytes, buf)?;

            while next_chunk <= end_chunk && join_set.len() < max_inflight {
                launch(next_chunk, &mut join_set);
                next_chunk += 1;
            }
        }

        Ok(())
    }

    async fn read_chunk_healing(
        &self,
        chunk_index: u64,
    ) -> Result<Vec<u8>, ChunkedStreamingDiskError> {
        match self.inner.cache.read_chunk(chunk_index)? {
            Some(bytes) => Ok(bytes),
            None => {
                // Metadata says the chunk is present but the data is missing/corrupt.
                // Heal by dropping the chunk from the downloaded set and re-fetching.
                let chunk_size = self.inner.manifest.chunk_size;
                let chunk_start = chunk_index.checked_mul(chunk_size).ok_or_else(|| {
                    ChunkedStreamingDiskError::Protocol("chunk offset overflow".to_string())
                })?;
                let chunk_end = chunk_start
                    .saturating_add(chunk_size)
                    .min(self.inner.manifest.total_size);
                {
                    let mut state = self.inner.state.lock().await;
                    state.downloaded.remove(chunk_start, chunk_end);
                }
                self.save_meta().await?;

                let token = self.inner.cancel_token.lock().await.clone();
                self.ensure_chunk_cached(chunk_index, &token).await?;
                self.inner.cache.read_chunk(chunk_index)?.ok_or_else(|| {
                    ChunkedStreamingDiskError::Io("chunk vanished after re-download".to_string())
                })
            }
        }
    }

    async fn ensure_chunk_cached(
        &self,
        chunk_index: u64,
        token: &CancellationToken,
    ) -> Result<(), ChunkedStreamingDiskError> {
        let chunk_size = self.inner.manifest.chunk_size;
        let Some(chunk_start) = chunk_index.checked_mul(chunk_size) else {
            return Ok(());
        };
        if chunk_start >= self.inner.manifest.total_size {
            return Ok(());
        }
        let chunk_end = chunk_start
            .saturating_add(chunk_size)
            .min(self.inner.manifest.total_size);

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
                .map_err(|_| ChunkedStreamingDiskError::Cancelled)?
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
    ) -> Result<(), ChunkedStreamingDiskError> {
        let _permit = tokio::select! {
            _ = token.cancelled() => return Err(ChunkedStreamingDiskError::Cancelled),
            permit = self.inner.fetch_sem.acquire() => permit.map_err(|_| ChunkedStreamingDiskError::Cancelled)?,
        };

        let bytes = self
            .fetch_with_retries(chunk_index, chunk_end - chunk_start, token)
            .await?;

        if token.is_cancelled() {
            return Err(ChunkedStreamingDiskError::Cancelled);
        }

        if bytes.len() as u64 != chunk_end - chunk_start {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "chunk {chunk_index} length mismatch: expected {} got {}",
                chunk_end - chunk_start,
                bytes.len()
            )));
        }

        if let Some(expected) = self.inner.manifest.sha256_for_chunk(chunk_index) {
            let actual = Sha256::digest(&bytes);
            let mut actual_arr = [0u8; 32];
            actual_arr.copy_from_slice(&actual);
            if actual_arr != expected {
                return Err(ChunkedStreamingDiskError::Integrity {
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

    async fn save_meta(&self) -> Result<(), ChunkedStreamingDiskError> {
        let _guard = self.inner.meta_write_lock.lock().await;
        let meta = {
            let state = self.inner.state.lock().await;
            CacheMeta {
                version: CACHE_META_VERSION,
                schema: MANIFEST_SCHEMA_V1.to_string(),
                manifest_version: self.inner.manifest.version.clone(),
                manifest_sha256: hex::encode(self.inner.manifest_sha256),
                total_size: self.inner.manifest.total_size,
                chunk_size: self.inner.manifest.chunk_size,
                chunk_count: self.inner.manifest.chunk_count,
                chunk_index_width: self.inner.manifest.chunk_index_width as u64,
                cache_backend: Some(self.inner.cache_backend),
                downloaded: state.downloaded.clone(),
            }
        };
        self.inner.meta_store.save(&meta)?;
        Ok(())
    }

    async fn fetch_with_retries(
        &self,
        chunk_index: u64,
        expected_len: u64,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, ChunkedStreamingDiskError> {
        let mut backoff = Duration::from_millis(100);
        let mut last_err = None;

        for attempt in 0..self.inner.options.max_retries {
            match self
                .fetch_chunk_once(chunk_index, expected_len, token)
                .await
            {
                Ok(bytes) => return Ok(bytes),
                Err(e) => {
                    let should_retry = match &e {
                        ChunkedStreamingDiskError::Integrity { .. }
                        | ChunkedStreamingDiskError::Protocol(_)
                        | ChunkedStreamingDiskError::Cancelled => false,
                        ChunkedStreamingDiskError::HttpStatus { status } => {
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

        Err(last_err.unwrap_or_else(|| ChunkedStreamingDiskError::Http("unknown".to_string())))
    }

    async fn fetch_chunk_once(
        &self,
        chunk_index: u64,
        expected_len: u64,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, ChunkedStreamingDiskError> {
        if expected_len == 0 {
            return Ok(Vec::new());
        }

        let url = chunk_url(
            &self.inner.manifest_url,
            self.inner.manifest.chunk_index_width,
            chunk_index,
        )
        .map_err(|e| ChunkedStreamingDiskError::Protocol(e.to_string()))?;

        self.inner
            .telemetry
            .http_gets
            .fetch_add(1, Ordering::Relaxed);

        let req = self
            .inner
            .client
            .get(url.clone())
            .headers(self.inner.request_headers.clone());

        let resp = tokio::select! {
            _ = token.cancelled() => return Err(ChunkedStreamingDiskError::Cancelled),
            resp = req.send() => resp.map_err(|e| ChunkedStreamingDiskError::Http(format_reqwest_error(e)))?,
        };

        if !resp.status().is_success() {
            return Err(ChunkedStreamingDiskError::HttpStatus {
                status: resp.status().as_u16(),
            });
        }

        if let Some(encoding) = resp
            .headers()
            .get(CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
        {
            let encoding = encoding.trim();
            if !encoding.eq_ignore_ascii_case("identity") {
                return Err(ChunkedStreamingDiskError::Protocol(format!(
                    "unexpected Content-Encoding: {encoding}"
                )));
            }
        }
        require_no_transform_cache_control(resp.headers(), &format!("chunk {chunk_index}"))?;

        let expected_usize: usize = expected_len.try_into().map_err(|_| {
            ChunkedStreamingDiskError::Protocol(format!(
                "expected chunk length {expected_len} does not fit in usize"
            ))
        })?;

        let bytes = read_response_bytes_with_limit(resp, expected_usize, token).await?;

        self.inner
            .telemetry
            .bytes_downloaded
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);

        Ok(bytes)
    }
}

async fn fetch_and_parse_manifest(
    client: &reqwest::Client,
    url: &Url,
    request_headers: &HeaderMap,
) -> Result<(ChunkedDiskManifestV1, [u8; 32]), ChunkedStreamingDiskError> {
    let req = client.get(url.clone()).headers(request_headers.clone());
    let resp = req
        .send()
        .await
        .map_err(|e| ChunkedStreamingDiskError::Http(format_reqwest_error(e)))?;
    if !resp.status().is_success() {
        return Err(ChunkedStreamingDiskError::HttpStatus {
            status: resp.status().as_u16(),
        });
    }

    if let Some(encoding) = resp
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
    {
        let encoding = encoding.trim();
        if !encoding.eq_ignore_ascii_case("identity") {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "unexpected Content-Encoding: {encoding}"
            )));
        }
    }
    require_no_transform_cache_control(resp.headers(), "manifest.json")?;

    let bytes =
        read_response_bytes_with_limit(resp, MAX_MANIFEST_JSON_BYTES, &CancellationToken::new())
            .await?;
    let digest = Sha256::digest(&bytes);
    let mut digest_arr = [0u8; 32];
    digest_arr.copy_from_slice(&digest);

    let raw: ManifestV1Raw = serde_json::from_slice(&bytes)?;
    let manifest = parse_manifest_v1(raw)?;
    Ok((manifest, digest_arr))
}

fn copy_from_chunk(
    chunk_index: u64,
    chunk_size: u64,
    read_start: u64,
    read_end: u64,
    bytes: &[u8],
    buf: &mut [u8],
) -> Result<(), ChunkedStreamingDiskError> {
    let chunk_start = chunk_index
        .checked_mul(chunk_size)
        .ok_or_else(|| ChunkedStreamingDiskError::Protocol("chunk offset overflow".to_string()))?;
    let chunk_end = chunk_start
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| ChunkedStreamingDiskError::Protocol("chunk end overflow".to_string()))?;

    let copy_start = read_start.max(chunk_start);
    let copy_end = read_end.min(chunk_end);
    if copy_end <= copy_start {
        return Ok(());
    }

    let src_start: usize = (copy_start - chunk_start).try_into().map_err(|_| {
        ChunkedStreamingDiskError::Protocol("copy offset does not fit in usize".to_string())
    })?;
    let dst_start: usize = (copy_start - read_start).try_into().map_err(|_| {
        ChunkedStreamingDiskError::Protocol("copy offset does not fit in usize".to_string())
    })?;
    let len: usize = (copy_end - copy_start).try_into().map_err(|_| {
        ChunkedStreamingDiskError::Protocol("copy length does not fit in usize".to_string())
    })?;

    buf[dst_start..dst_start + len].copy_from_slice(&bytes[src_start..src_start + len]);
    Ok(())
}

fn chunk_url(
    manifest_url: &Url,
    chunk_index_width: usize,
    chunk_index: u64,
) -> Result<Url, url::ParseError> {
    let name = format!("{:0width$}.bin", chunk_index, width = chunk_index_width);
    let mut url = manifest_url.join(&format!("chunks/{name}"))?;
    // Preserve querystring auth material from the manifest URL (e.g. signed URLs).
    url.set_query(manifest_url.query());
    url.set_fragment(None);
    Ok(url)
}

async fn read_response_bytes_with_limit(
    mut resp: reqwest::Response,
    max_bytes: usize,
    token: &CancellationToken,
) -> Result<Vec<u8>, ChunkedStreamingDiskError> {
    let mut out = Vec::new();
    if max_bytes > 0 {
        out.reserve(max_bytes.min(1024));
    }

    while let Some(chunk) = tokio::select! {
        _ = token.cancelled() => return Err(ChunkedStreamingDiskError::Cancelled),
        c = resp.chunk() => c.map_err(|e| ChunkedStreamingDiskError::Http(format_reqwest_error(e)))?,
    } {
        if out.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "response too large (max {max_bytes} bytes)"
            )));
        }
        out.extend_from_slice(&chunk);
    }

    Ok(out)
}

fn build_header_map(headers: &[(String, String)]) -> Result<HeaderMap, ChunkedStreamingDiskError> {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let name_lower = name.to_ascii_lowercase();
        let name = HeaderName::from_bytes(name_lower.as_bytes())
            .map_err(|e| ChunkedStreamingDiskError::Protocol(e.to_string()))?;
        let value = HeaderValue::from_str(value)
            .map_err(|e| ChunkedStreamingDiskError::Protocol(e.to_string()))?;
        out.insert(name, value);
    }
    Ok(out)
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

fn redact_url_for_logs(url: &Url) -> Url {
    let mut url = url.clone();
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

// We use `hex` only for integrity error messages and cache identity. Keep it private to avoid
// committing to a public dependency in the API surface.
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

/// Synchronous wrapper around [`ChunkedStreamingDisk`] for use in native device-model code.
///
/// `ChunkedStreamingDisk` itself is async because it performs network fetches. Many storage
/// controllers in Aero are currently synchronous and consume a `VirtualDisk`-like interface.
/// `ChunkedStreamingDiskSync` bridges that gap by running an internal Tokio runtime and exposing
/// blocking read methods.
///
/// Note: The blocking methods should not be called from within an existing Tokio runtime on the
/// same thread. In async contexts, prefer `tokio::task::spawn_blocking` or use the async
/// [`ChunkedStreamingDisk`] directly.
pub struct ChunkedStreamingDiskSync {
    rt: tokio::runtime::Runtime,
    disk: ChunkedStreamingDisk,
}

impl ChunkedStreamingDiskSync {
    pub fn open(config: ChunkedStreamingDiskConfig) -> Result<Self, ChunkedStreamingDiskError> {
        // Use a current-thread runtime to avoid spawning extra worker threads in typical
        // synchronous emulator/device contexts. The internal async implementation performs
        // I/O using non-blocking sockets and can still overlap requests.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| ChunkedStreamingDiskError::Io(e.to_string()))?;
        let disk = rt.block_on(ChunkedStreamingDisk::open(config))?;
        Ok(Self { rt, disk })
    }

    pub fn total_size(&self) -> u64 {
        self.disk.manifest().total_size
    }

    pub fn manifest(&self) -> &ChunkedDiskManifestV1 {
        self.disk.manifest()
    }

    pub fn telemetry_snapshot(&self) -> ChunkedStreamingTelemetrySnapshot {
        self.disk.telemetry_snapshot()
    }

    pub fn read_at(
        &mut self,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), ChunkedStreamingDiskError> {
        self.rt.block_on(self.disk.read_at(offset, buf))
    }

    pub fn read_sectors(
        &mut self,
        lba: u64,
        buf: &mut [u8],
    ) -> Result<(), ChunkedStreamingDiskError> {
        if !(buf.len() as u64).is_multiple_of(SECTOR_SIZE_BYTES) {
            return Err(ChunkedStreamingDiskError::Protocol(format!(
                "read_sectors buffer length must be multiple of {SECTOR_SIZE_BYTES}"
            )));
        }
        let offset = lba
            .checked_mul(SECTOR_SIZE_BYTES)
            .ok_or_else(|| ChunkedStreamingDiskError::Protocol("lba overflow".to_string()))?;
        self.read_at(offset, buf)
    }

    pub fn flush(&mut self) -> Result<(), ChunkedStreamingDiskError> {
        self.rt.block_on(self.disk.flush())
    }

    pub fn reset(&mut self) -> Result<(), ChunkedStreamingDiskError> {
        self.rt.block_on(self.disk.reset());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest_raw() -> ManifestV1Raw {
        ManifestV1Raw {
            schema: MANIFEST_SCHEMA_V1.to_string(),
            version: "v1".to_string(),
            mime_type: "application/octet-stream".to_string(),
            total_size: 2 * SECTOR_SIZE_BYTES,
            chunk_size: SECTOR_SIZE_BYTES,
            chunk_count: 2,
            chunk_index_width: 1,
            chunks: None,
        }
    }

    #[test]
    fn parse_hex_sha256_accepts_uppercase_and_trims() {
        let input = format!("  {}  ", "A".repeat(64));
        let parsed = parse_hex_sha256(&input).expect("expected hex parsing to succeed");
        assert_eq!(parsed, [0xaa; 32]);
    }

    #[test]
    fn parse_hex_sha256_rejects_invalid_length() {
        let err = parse_hex_sha256("00").expect_err("expected length validation failure");
        assert!(
            matches!(
                &err,
                ChunkedStreamingDiskError::Protocol(msg) if msg.contains("64-char")
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn parse_manifest_v1_accepts_minimal_manifest() {
        let manifest = parse_manifest_v1(valid_manifest_raw()).expect("expected manifest to parse");
        assert_eq!(manifest.version, "v1");
        assert_eq!(manifest.mime_type, "application/octet-stream");
        assert_eq!(manifest.total_size, 2 * SECTOR_SIZE_BYTES);
        assert_eq!(manifest.chunk_size, SECTOR_SIZE_BYTES);
        assert_eq!(manifest.chunk_count, 2);
        assert_eq!(manifest.chunk_index_width, 1);
        assert_eq!(manifest.chunk_sha256.len(), 2);
        assert!(manifest.chunk_sha256.iter().all(|v| v.is_none()));
    }

    #[test]
    fn parse_manifest_v1_parses_optional_chunk_sha256_list() {
        let mut raw = valid_manifest_raw();
        raw.chunks = Some(vec![
            ChunkEntryRaw {
                size: Some(SECTOR_SIZE_BYTES),
                sha256: Some("0".repeat(64)),
            },
            ChunkEntryRaw {
                size: Some(SECTOR_SIZE_BYTES),
                sha256: None,
            },
        ]);
        let manifest = parse_manifest_v1(raw).expect("expected manifest to parse");
        assert_eq!(manifest.chunk_sha256.len(), 2);
        assert_eq!(manifest.chunk_sha256[0], Some([0u8; 32]));
        assert_eq!(manifest.chunk_sha256[1], None);
    }

    #[test]
    fn parse_manifest_v1_rejects_unknown_schema() {
        let mut raw = valid_manifest_raw();
        raw.schema = "not-a-schema".to_string();
        let err = parse_manifest_v1(raw).expect_err("expected schema mismatch");
        assert!(
            matches!(
                &err,
                ChunkedStreamingDiskError::UnsupportedManifestSchema(_)
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn parse_manifest_v1_rejects_chunk_count_mismatch() {
        let mut raw = valid_manifest_raw();
        raw.chunk_count = 3;
        let err = parse_manifest_v1(raw).expect_err("expected chunkCount mismatch");
        assert!(
            matches!(
                &err,
                ChunkedStreamingDiskError::Protocol(msg) if msg.contains("chunkCount mismatch")
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn parse_manifest_v1_rejects_chunk_index_width_too_small() {
        let mut raw = valid_manifest_raw();
        raw.total_size = SECTOR_SIZE_BYTES * 100;
        raw.chunk_count = 100;
        raw.chunk_index_width = 1;
        let err = parse_manifest_v1(raw).expect_err("expected chunkIndexWidth too small");
        assert!(
            matches!(
                &err,
                ChunkedStreamingDiskError::Protocol(msg)
                    if msg.contains("chunkIndexWidth too small")
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn parse_manifest_v1_rejects_chunks_length_mismatch() {
        let mut raw = valid_manifest_raw();
        raw.chunks = Some(vec![ChunkEntryRaw {
            size: Some(SECTOR_SIZE_BYTES),
            sha256: None,
        }]);
        let err = parse_manifest_v1(raw).expect_err("expected chunks length mismatch");
        assert!(
            matches!(
                &err,
                ChunkedStreamingDiskError::Protocol(msg) if msg.contains("chunks.length mismatch")
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn parse_manifest_v1_rejects_chunk_size_mismatch_in_chunks_list() {
        let mut raw = valid_manifest_raw();
        raw.chunks = Some(vec![
            ChunkEntryRaw {
                size: Some(511),
                sha256: None,
            },
            ChunkEntryRaw {
                size: Some(SECTOR_SIZE_BYTES),
                sha256: None,
            },
        ]);
        let err = parse_manifest_v1(raw).expect_err("expected chunk size mismatch");
        assert!(
            matches!(
                &err,
                ChunkedStreamingDiskError::Protocol(msg)
                    if msg.contains("chunks[0].size mismatch")
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn parse_manifest_v1_rejects_invalid_chunk_sha256() {
        let mut raw = valid_manifest_raw();
        raw.chunks = Some(vec![
            ChunkEntryRaw {
                size: Some(SECTOR_SIZE_BYTES),
                sha256: Some("deadbeef".to_string()),
            },
            ChunkEntryRaw {
                size: Some(SECTOR_SIZE_BYTES),
                sha256: None,
            },
        ]);
        let err = parse_manifest_v1(raw).expect_err("expected sha256 validation failure");
        assert!(
            matches!(
                &err,
                ChunkedStreamingDiskError::Protocol(msg) if msg.contains("chunks[0].sha256")
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn redact_url_for_logs_removes_query_fragment_and_credentials() {
        let url: Url = "https://user:pass@example.com/manifest.json?token=secret#frag"
            .parse()
            .expect("parse url");
        let redacted = redact_url_for_logs(&url);
        assert_eq!(redacted.query(), None);
        assert_eq!(redacted.fragment(), None);
        assert_eq!(redacted.username(), "");
        assert!(redacted.password().is_none());
        assert!(!redacted.as_str().contains("token="));
        assert!(!redacted.as_str().contains("secret"));
        assert!(!redacted.as_str().contains("user"));
        assert!(!redacted.as_str().contains("pass"));
    }

    #[test]
    fn chunked_streaming_disk_config_debug_redacts_secrets() {
        let url: Url = "https://user:pass@example.com/manifest.json?token=secret"
            .parse()
            .expect("parse url");
        let mut config = ChunkedStreamingDiskConfig::new(url, "cache-dir");
        config.request_headers = vec![(
            "Authorization".to_string(),
            "Bearer super-secret-token".to_string(),
        )];

        let dbg = format!("{config:?}");
        assert!(
            !dbg.contains("token=secret"),
            "expected querystring to be redacted; debug was: {dbg}"
        );
        assert!(
            dbg.contains("username: \"\"") && dbg.contains("password: None"),
            "expected credentials to be redacted; debug was: {dbg}"
        );
        assert!(
            dbg.contains("Authorization") || dbg.to_ascii_lowercase().contains("authorization"),
            "expected header name to be listed; debug was: {dbg}"
        );
        assert!(
            !dbg.contains("super-secret-token"),
            "expected header value to be redacted; debug was: {dbg}"
        );
    }

    #[test]
    fn build_header_map_normalizes_names_and_rejects_invalid_values() {
        let headers = vec![
            ("Authorization".to_string(), "Bearer ok".to_string()),
            // Values cannot include newlines.
            ("X-Test".to_string(), "ok\r\nInjected: bad".to_string()),
        ];
        let err = build_header_map(&headers).expect_err("expected invalid header value error");
        assert!(
            matches!(&err, ChunkedStreamingDiskError::Protocol(_)),
            "unexpected error: {err:?}"
        );

        let ok_headers = vec![("Authorization".to_string(), "Bearer ok".to_string())];
        let map = build_header_map(&ok_headers).expect("expected header map build to succeed");
        assert!(
            map.get("authorization").is_some(),
            "expected header names to be normalized"
        );
    }
}
