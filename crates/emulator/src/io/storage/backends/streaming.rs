use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::io::storage::{
    error::StorageError,
    metadata::{MetadataStore, StreamingMetadata},
    rangeset::RangeSet,
    sparse::SparseStore,
    SECTOR_SIZE,
};

#[derive(Clone, Debug)]
pub struct ChunkManifest {
    pub chunk_size: u64,
    pub sha256: Vec<[u8; 32]>,
}

impl ChunkManifest {
    pub fn sha256_for_chunk(&self, chunk_index: u64) -> Option<[u8; 32]> {
        self.sha256.get(chunk_index as usize).copied()
    }
}

#[derive(Clone, Debug)]
pub struct StreamingDiskOptions {
    /// Caching unit for the remote base image. All range fetches are chunk-aligned.
    pub chunk_size: u64,
    /// How many *chunks* to prefetch when sequential reads are detected.
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
            chunk_size: 1024 * 1024,
            read_ahead_chunks: 2,
            max_concurrent_fetches: 4,
            max_retries: 4,
            manifest: None,
        }
    }
}

#[derive(Default)]
pub struct StreamingTelemetry {
    pub bytes_downloaded: AtomicU64,
    pub range_requests: AtomicU64,
    pub cache_hit_chunks: AtomicU64,
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

pub struct StreamingDisk {
    inner: Arc<StreamingDiskInner>,
}

struct StreamingDiskInner {
    client: reqwest::Client,
    url: String,
    size: u64,
    cache: Arc<dyn SparseStore>,
    overlay: Arc<dyn SparseStore>,
    metadata: Arc<dyn MetadataStore>,
    options: StreamingDiskOptions,
    telemetry: StreamingTelemetry,
    fetch_sem: Semaphore,
    cancel_token: Mutex<CancellationToken>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    downloaded: RangeSet,
    dirty: RangeSet,
    in_flight: HashMap<u64, Vec<oneshot::Sender<Result<(), StorageError>>>>,
    last_read_end: Option<u64>,
}

impl StreamingDisk {
    pub async fn new(
        url: impl Into<String>,
        cache: Arc<dyn SparseStore>,
        overlay: Arc<dyn SparseStore>,
        metadata: Arc<dyn MetadataStore>,
        options: StreamingDiskOptions,
    ) -> Result<Self, StorageError> {
        let url = url.into();
        let client = reqwest::Client::new();

        if options.chunk_size == 0 || options.chunk_size % SECTOR_SIZE as u64 != 0 {
            return Err(StorageError::Protocol(format!(
                "chunk_size must be a non-zero multiple of sector size ({SECTOR_SIZE})"
            )));
        }
        if options.max_retries == 0 {
            return Err(StorageError::Protocol(
                "max_retries must be greater than zero".to_string(),
            ));
        }

        let size = probe_remote_size_and_range_support(&client, &url).await?;

        if cache.size() != size {
            return Err(StorageError::Protocol(format!(
                "cache size mismatch (cache={}, remote={size})",
                cache.size()
            )));
        }
        if overlay.size() != size {
            return Err(StorageError::Protocol(format!(
                "overlay size mismatch (overlay={}, remote={size})",
                overlay.size()
            )));
        }

        if let Some(manifest) = &options.manifest {
            if manifest.chunk_size != options.chunk_size {
                return Err(StorageError::Protocol(format!(
                    "manifest chunk_size ({}) does not match options.chunk_size ({})",
                    manifest.chunk_size, options.chunk_size
                )));
            }

            let expected_chunks = ((size + options.chunk_size - 1) / options.chunk_size) as usize;
            if manifest.sha256.len() != expected_chunks {
                return Err(StorageError::Protocol(format!(
                    "manifest chunk count ({}) does not match expected ({expected_chunks})",
                    manifest.sha256.len()
                )));
            }
        }

        let mut state = State::default();
        if let Some(meta) = metadata.load()? {
            state.downloaded = meta.downloaded;
            state.dirty = meta.dirty;
        }

        Ok(Self {
            inner: Arc::new(StreamingDiskInner {
                client,
                url,
                size,
                cache,
                overlay,
                metadata,
                options: options.clone(),
                telemetry: StreamingTelemetry::default(),
                fetch_sem: Semaphore::new(options.max_concurrent_fetches.max(1)),
                cancel_token: Mutex::new(CancellationToken::new()),
                state: Mutex::new(state),
            }),
        })
    }

    pub fn size(&self) -> u64 {
        self.inner.size
    }

    pub fn telemetry_snapshot(&self) -> StreamingTelemetrySnapshot {
        self.inner.telemetry.snapshot()
    }

    pub async fn reset(&self) {
        // Cancel all outstanding prefetch/fetch work and fail any waiters.
        {
            let mut token = self.inner.cancel_token.lock().await;
            token.cancel();
            *token = CancellationToken::new();
        };

        // Drain waiters so callers don't hang forever if a reset happens mid-download.
        let mut state = self.inner.state.lock().await;
        let waiters = std::mem::take(&mut state.in_flight);
        state.last_read_end = None;
        drop(state);

        for (_, senders) in waiters {
            for sender in senders {
                let _ = sender.send(Err(StorageError::Cancelled));
            }
        }
    }

    pub async fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        if buf.len() % SECTOR_SIZE != 0 {
            return Err(StorageError::Protocol(format!(
                "read buffer length must be multiple of {SECTOR_SIZE}"
            )));
        }
        let offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or_else(|| StorageError::Protocol("lba overflow".to_string()))?;
        self.read_at(offset, buf).await
    }

    pub async fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), StorageError> {
        if buf.len() % SECTOR_SIZE != 0 {
            return Err(StorageError::Protocol(format!(
                "write buffer length must be multiple of {SECTOR_SIZE}"
            )));
        }
        let offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or_else(|| StorageError::Protocol("lba overflow".to_string()))?;
        self.write_at(offset, buf).await
    }

    pub async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        let len = buf.len() as u64;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| StorageError::Protocol("read overflow".to_string()))?;
        if end > self.inner.size {
            return Err(StorageError::OutOfBounds {
                offset,
                len,
                size: self.inner.size,
            });
        }

        let token = self.inner.cancel_token.lock().await.clone();
        let read_ahead_chunks = self.inner.options.read_ahead_chunks;
        let chunk_size = self.inner.options.chunk_size;

        let (dirty_ranges, base_needed, sequential) = {
            let mut state = self.inner.state.lock().await;
            let dirty_ranges = state.dirty.intersecting(offset, end);
            let base_needed = state.dirty.gaps(offset, end);
            let sequential = state.last_read_end.map_or(true, |prev| prev == offset);
            state.last_read_end = Some(end);
            (dirty_ranges, base_needed, sequential)
        };

        for missing in base_needed {
            self.ensure_range_cached(missing.start, missing.end, &token)
                .await?;
        }

        // Fill from base cache first. Any bytes not downloaded are expected to be overwritten
        // by the overlay (dirty ranges), so reading zeros from holes is acceptable.
        self.inner.cache.read_at(offset, buf)?;

        // Overlay guest writes.
        for seg in dirty_ranges {
            let mut tmp = vec![0u8; seg.len() as usize];
            self.inner.overlay.read_at(seg.start, &mut tmp)?;
            let dst_start = (seg.start - offset) as usize;
            buf[dst_start..dst_start + tmp.len()].copy_from_slice(&tmp);
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

    pub async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
        let len = buf.len() as u64;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| StorageError::Protocol("write overflow".to_string()))?;
        if end > self.inner.size {
            return Err(StorageError::OutOfBounds {
                offset,
                len,
                size: self.inner.size,
            });
        }

        self.inner.overlay.write_at(offset, buf)?;

        let meta = {
            let mut state = self.inner.state.lock().await;
            state.dirty.insert(offset, end);
            StreamingMetadata {
                downloaded: state.downloaded.clone(),
                dirty: state.dirty.clone(),
            }
        };

        self.inner.metadata.save(&meta)?;
        Ok(())
    }

    pub async fn flush(&self) -> Result<(), StorageError> {
        self.inner.cache.flush()?;
        self.inner.overlay.flush()?;

        let meta = {
            let state = self.inner.state.lock().await;
            StreamingMetadata {
                downloaded: state.downloaded.clone(),
                dirty: state.dirty.clone(),
            }
        };
        self.inner.metadata.save(&meta)?;
        Ok(())
    }

    fn spawn_prefetch(&self, start_chunk: u64, count: u64, token: CancellationToken) {
        let disk = self.clone();
        tokio::spawn(async move {
            for chunk in start_chunk..start_chunk.saturating_add(count) {
                if token.is_cancelled() {
                    break;
                }
                if chunk * disk.inner.options.chunk_size >= disk.inner.size {
                    break;
                }
                let _ = disk.ensure_chunk_cached(chunk, &token).await;
            }
        });
    }

    async fn ensure_range_cached(
        &self,
        start: u64,
        end: u64,
        token: &CancellationToken,
    ) -> Result<(), StorageError> {
        if start >= end {
            return Ok(());
        }

        let chunk_size = self.inner.options.chunk_size;
        let first = start / chunk_size;
        let last = (end.saturating_sub(1)) / chunk_size;
        for chunk in first..=last {
            self.ensure_chunk_cached(chunk, token).await?;
        }
        Ok(())
    }

    async fn ensure_chunk_cached(
        &self,
        chunk_index: u64,
        token: &CancellationToken,
    ) -> Result<(), StorageError> {
        let chunk_start = chunk_index * self.inner.options.chunk_size;
        if chunk_start >= self.inner.size {
            return Ok(());
        }
        let chunk_end = (chunk_start + self.inner.options.chunk_size).min(self.inner.size);

        // Fast path: already downloaded.
        {
            let state = self.inner.state.lock().await;
            if state.downloaded.contains_range(chunk_start, chunk_end) {
                self.inner
                    .telemetry
                    .cache_hit_chunks
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }

        self.inner
            .telemetry
            .cache_miss_chunks
            .fetch_add(1, Ordering::Relaxed);

        // Either wait for an in-flight download or become the leader and fetch ourselves.
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
                None
            }
        };

        if let Some(rx) = waiter_rx {
            return rx
                .await
                .map_err(|_| StorageError::Cancelled)?
                .map(|_| ());
        }

        // We're the leader for this chunk.
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
    ) -> Result<(), StorageError> {
        let _permit = tokio::select! {
            _ = token.cancelled() => return Err(StorageError::Cancelled),
            permit = self.inner.fetch_sem.acquire() => permit.map_err(|_| StorageError::Cancelled)?,
        };

        let bytes = self
            .fetch_with_retries(chunk_start, chunk_end, token)
            .await?;

        if token.is_cancelled() {
            return Err(StorageError::Cancelled);
        }

        if bytes.len() as u64 != chunk_end - chunk_start {
            return Err(StorageError::Protocol(format!(
                "short read: expected {} bytes, got {}",
                chunk_end - chunk_start,
                bytes.len()
            )));
        }

        if let Some(manifest) = &self.inner.options.manifest {
            if let Some(expected) = manifest.sha256_for_chunk(chunk_index) {
                let actual = Sha256::digest(&bytes);
                let mut actual_arr = [0u8; 32];
                actual_arr.copy_from_slice(&actual);
                if actual_arr != expected {
                    return Err(StorageError::Integrity {
                        chunk_index,
                        expected: hex::encode(expected),
                        actual: hex::encode(actual_arr),
                    });
                }
            }
        }

        self.inner.cache.write_at(chunk_start, &bytes)?;

        let meta = {
            let mut state = self.inner.state.lock().await;
            state.downloaded.insert(chunk_start, chunk_end);
            StreamingMetadata {
                downloaded: state.downloaded.clone(),
                dirty: state.dirty.clone(),
            }
        };

        self.inner.metadata.save(&meta)?;
        Ok(())
    }

    async fn fetch_with_retries(
        &self,
        start: u64,
        end: u64,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, StorageError> {
        let mut backoff = Duration::from_millis(100);
        let mut last_err = None;

        for attempt in 0..self.inner.options.max_retries {
            match self.fetch_range_once(start, end, token).await {
                Ok(bytes) => return Ok(bytes),
                Err(e) => {
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

        Err(last_err.unwrap_or_else(|| StorageError::Http("unknown".to_string())))
    }

    async fn fetch_range_once(
        &self,
        start: u64,
        end: u64,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, StorageError> {
        if start >= end {
            return Ok(Vec::new());
        }

        let range_header = format!("bytes={}-{}", start, end - 1);
        self.inner
            .telemetry
            .range_requests
            .fetch_add(1, Ordering::Relaxed);

        let resp = tokio::select! {
            _ = token.cancelled() => return Err(StorageError::Cancelled),
            resp = self.inner.client.get(&self.inner.url).header(RANGE, range_header).send() => {
                resp.map_err(|e| StorageError::Http(e.to_string()))?
            }
        };

        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            if resp.status().is_success() {
                return Err(StorageError::RangeNotSupported);
            }
            return Err(StorageError::Http(format!(
                "unexpected status {}",
                resp.status()
            )));
        }

        let bytes = tokio::select! {
            _ = token.cancelled() => return Err(StorageError::Cancelled),
            bytes = resp.bytes() => bytes.map_err(|e| StorageError::Http(e.to_string()))?
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

async fn probe_remote_size_and_range_support(
    client: &reqwest::Client,
    url: &str,
) -> Result<u64, StorageError> {
    // Prefer HEAD to discover content-length quickly.
    let head = client.head(url).send().await;
    if let Ok(resp) = head {
        if resp.status().is_success() {
            let accept_ranges = resp
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_ascii_lowercase());

            let len = resp
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            if accept_ranges.as_deref() == Some("bytes") {
                if let Some(len) = len {
                    return Ok(len);
                }
            }
        }
    }

    // Fall back to a 0-0 range request; parse Content-Range: "bytes 0-0/12345".
    let resp = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| StorageError::Http(e.to_string()))?;

    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(StorageError::RangeNotSupported);
    }

    let cr = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| StorageError::Protocol("missing Content-Range".to_string()))?;

    // Minimal parse for `bytes 0-0/12345`.
    let total = cr
        .split('/')
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| StorageError::Protocol(format!("invalid Content-Range: {cr}")))?;

    Ok(total)
}

// We use `hex` only for integrity error messages. Keep it private to avoid
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
