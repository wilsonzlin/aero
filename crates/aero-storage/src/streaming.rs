use crate::range_set::{ByteRange, RangeSet};
use bytes::Bytes;
use hyper::body::HttpBody;
use hyper::client::HttpConnector;
use hyper::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use hyper::{Body, Client, Method, Request, StatusCode, Uri};
use hyper_rustls::HttpsConnectorBuilder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use url::Url;

const META_FILE_NAME: &str = "streaming-cache-meta.json";
const BLOCKS_DIR_NAME: &str = "blocks";
pub const DEFAULT_SECTOR_SIZE: u64 = 512;
pub const DEFAULT_BLOCK_SIZE: u64 = 1024 * 1024; // 1MiB

#[derive(Debug, Clone)]
pub struct PrefetchConfig {
    pub enabled: bool,
    pub sequential_distance_blocks: u64,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sequential_distance_blocks: 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamingDiskConfig {
    pub url: Url,
    pub cache_dir: PathBuf,
    pub block_size: u64,
    pub cache_limit_bytes: Option<u64>,
    pub prefetch: PrefetchConfig,
}

impl StreamingDiskConfig {
    pub fn new(url: Url, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            url,
            cache_dir: cache_dir.into(),
            block_size: DEFAULT_BLOCK_SIZE,
            cache_limit_bytes: Some(512 * 1024 * 1024), // 512MiB default
            prefetch: PrefetchConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStatus {
    pub total_size: u64,
    pub cached_bytes: u64,
    pub cached_ranges: Vec<ByteRange>,
    pub cache_limit_bytes: Option<u64>,
}

#[derive(Debug, Error)]
pub enum StreamingDiskError {
    #[error("remote server does not appear to support HTTP Range requests (required for streaming): {0}")]
    RangeNotSupported(String),
    #[error("remote server did not provide a valid Content-Length")]
    MissingContentLength,
    #[error("unexpected HTTP response: {status} {reason}")]
    UnexpectedHttpResponse { status: u16, reason: String },
    #[error("invalid Content-Range header: {0}")]
    InvalidContentRange(String),
    #[error("unexpected range response length: expected {expected} bytes, got {actual} bytes")]
    UnexpectedRangeLength { expected: usize, actual: usize },
    #[error("URL must be absolute: {0}")]
    UrlNotAbsolute(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Hyper(#[from] hyper::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheMeta {
    version: u32,
    url: String,
    total_size: u64,
    block_size: u64,
    downloaded: RangeSet,
    access_counter: u64,
    block_last_access: HashMap<u64, u64>,
}

impl CacheMeta {
    fn new(url: &Url, total_size: u64, block_size: u64) -> Self {
        Self {
            version: 1,
            url: url.to_string(),
            total_size,
            block_size,
            downloaded: RangeSet::new(),
            access_counter: 0,
            block_last_access: HashMap::new(),
        }
    }
}

struct CacheState {
    meta: CacheMeta,
    meta_path: PathBuf,
    blocks_dir: PathBuf,
    cache_limit_bytes: Option<u64>,
}

impl CacheState {
    fn status(&self) -> CacheStatus {
        CacheStatus {
            total_size: self.meta.total_size,
            cached_bytes: self.meta.downloaded.total_len(),
            cached_ranges: self.meta.downloaded.ranges().to_vec(),
            cache_limit_bytes: self.cache_limit_bytes,
        }
    }

    fn block_path(&self, block_index: u64) -> PathBuf {
        self.blocks_dir.join(format!("{block_index}.bin"))
    }

    fn block_range(&self, block_index: u64) -> ByteRange {
        let start = block_index * self.meta.block_size;
        let end = (start + self.meta.block_size).min(self.meta.total_size);
        ByteRange { start, end }
    }

    fn is_block_cached(&self, block_index: u64) -> bool {
        let r = self.block_range(block_index);
        self.meta.downloaded.contains_range(r.start, r.end)
    }

    async fn load(
        cache_dir: &Path,
        url: &Url,
        total_size: u64,
        block_size: u64,
        cache_limit_bytes: Option<u64>,
    ) -> Result<Self, StreamingDiskError> {
        tokio::fs::create_dir_all(cache_dir).await?;
        let blocks_dir = cache_dir.join(BLOCKS_DIR_NAME);
        tokio::fs::create_dir_all(&blocks_dir).await?;
        let meta_path = cache_dir.join(META_FILE_NAME);

        let mut meta = match tokio::fs::read(&meta_path).await {
            Ok(bytes) => serde_json::from_slice::<CacheMeta>(&bytes)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                CacheMeta::new(url, total_size, block_size)
            }
            Err(err) => return Err(err.into()),
        };

        // Reject incompatible metadata. We conservatively reset if anything
        // important changed (different image, different block size, etc).
        let compatible = meta.version == 1
            && meta.url == url.to_string()
            && meta.total_size == total_size
            && meta.block_size == block_size;

        if !compatible {
            // Best-effort cleanup: remove the existing blocks directory and meta.
            let _ = tokio::fs::remove_dir_all(&blocks_dir).await;
            tokio::fs::create_dir_all(&blocks_dir).await?;
            let _ = tokio::fs::remove_file(&meta_path).await;
            meta = CacheMeta::new(url, total_size, block_size);
        }

        Ok(Self {
            meta,
            meta_path,
            blocks_dir,
            cache_limit_bytes,
        })
    }

    async fn persist(&self) -> Result<(), StreamingDiskError> {
        let tmp = self.meta_path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.meta)?;
        tokio::fs::write(&tmp, bytes).await?;
        match tokio::fs::rename(&tmp, &self.meta_path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                tokio::fs::remove_file(&self.meta_path).await?;
                tokio::fs::rename(&tmp, &self.meta_path).await?;
            }
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    fn note_access(&mut self, block_index: u64) {
        self.meta.access_counter = self.meta.access_counter.wrapping_add(1);
        self.meta
            .block_last_access
            .insert(block_index, self.meta.access_counter);
    }

    async fn read_block(&mut self, block_index: u64) -> Result<Option<Vec<u8>>, StreamingDiskError> {
        if !self.is_block_cached(block_index) {
            return Ok(None);
        }

        let path = self.block_path(block_index);
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Metadata says it's cached, but the data is missing. Heal by
                // dropping the range.
                let r = self.block_range(block_index);
                self.meta.downloaded.remove(r.start, r.end);
                self.meta.block_last_access.remove(&block_index);
                self.persist().await?;
                return Ok(None);
            }
            Err(err) => return Err(err.into()),
        };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;
        self.note_access(block_index);
        self.persist().await?;
        Ok(Some(buf))
    }

    async fn write_block(&mut self, block_index: u64, data: &[u8]) -> Result<(), StreamingDiskError> {
        let expected_len = self.block_range(block_index).len() as usize;
        if data.len() != expected_len {
            return Err(StreamingDiskError::UnexpectedRangeLength {
                expected: expected_len,
                actual: data.len(),
            });
        }

        let path = self.block_path(block_index);
        let tmp = path.with_extension("bin.tmp");
        tokio::fs::write(&tmp, data).await?;
        match tokio::fs::rename(&tmp, &path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                tokio::fs::remove_file(&path).await?;
                tokio::fs::rename(&tmp, &path).await?;
            }
            Err(err) => return Err(err.into()),
        }

        let r = self.block_range(block_index);
        self.meta.downloaded.insert(r.start, r.end);
        self.note_access(block_index);
        self.persist().await?;
        self.enforce_cache_limit(block_index).await?;
        Ok(())
    }

    async fn enforce_cache_limit(&mut self, protected_block: u64) -> Result<(), StreamingDiskError> {
        let Some(limit) = self.cache_limit_bytes else {
            return Ok(());
        };

        // If the limit can't fit even a single block, caching is effectively
        // disabled but we still keep the most recently accessed block.
        while self.meta.downloaded.total_len() > limit {
            // Find least-recently-used block (excluding the protected block).
            let mut lru_block = None;
            let mut lru_counter = u64::MAX;
            for (&block, &counter) in &self.meta.block_last_access {
                if block == protected_block {
                    continue;
                }
                if counter < lru_counter {
                    lru_counter = counter;
                    lru_block = Some(block);
                }
            }

            let Some(block_to_evict) = lru_block else {
                // Nothing left to evict without breaking the caller's read.
                break;
            };

            let path = self.block_path(block_to_evict);
            let _ = tokio::fs::remove_file(&path).await;
            let r = self.block_range(block_to_evict);
            self.meta.downloaded.remove(r.start, r.end);
            self.meta.block_last_access.remove(&block_to_evict);
            self.persist().await?;
        }

        Ok(())
    }
}

pub struct StreamingDisk {
    url: Url,
    total_size: u64,
    block_size: u64,
    client: Client<hyper_rustls::HttpsConnector<HttpConnector>>,
    cache: Mutex<CacheState>,
    last_read_end: Mutex<Option<u64>>,
    prefetch: PrefetchConfig,
}

impl StreamingDisk {
    pub async fn open(config: StreamingDiskConfig) -> Result<Self, StreamingDiskError> {
        if !config.url.has_host() {
            return Err(StreamingDiskError::UrlNotAbsolute(config.url.to_string()));
        }

        let client = Self::build_client();
        let (total_size, range_supported) = probe_range_support(&client, &config.url).await?;
        if !range_supported {
            return Err(StreamingDiskError::RangeNotSupported(config.url.to_string()));
        }

        let cache_state = CacheState::load(
            &config.cache_dir,
            &config.url,
            total_size,
            config.block_size,
            config.cache_limit_bytes,
        )
        .await?;

        Ok(Self {
            url: config.url,
            total_size,
            block_size: config.block_size,
            client,
            cache: Mutex::new(cache_state),
            last_read_end: Mutex::new(None),
            prefetch: config.prefetch,
        })
    }

    fn build_client() -> Client<hyper_rustls::HttpsConnector<HttpConnector>> {
        let https = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Client::builder().build::<_, Body>(https)
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    pub async fn cache_status(&self) -> CacheStatus {
        let cache = self.cache.lock().await;
        cache.status()
    }

    /// Read bytes at `offset` into `buf`.
    ///
    /// This method fetches blocks via HTTP Range requests on demand and stores
    /// them in the local cache directory.
    pub async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StreamingDiskError> {
        if buf.is_empty() {
            self.maybe_prefetch(offset, 0).await;
            return Ok(());
        }

        if offset.saturating_add(buf.len() as u64) > self.total_size {
            return Err(StreamingDiskError::UnexpectedHttpResponse {
                status: 416,
                reason: "read beyond end of image".to_string(),
            });
        }

        let (start_block, end_block) =
            block_span_for_range(offset, buf.len() as u64, self.block_size)
                .expect("non-empty buffer produces a span");

        let mut written = 0usize;
        for block_index in start_block..=end_block {
            let block_bytes = self.get_block(block_index).await?;
            let block_start = block_index * self.block_size;
            let in_block_start = if offset > block_start {
                (offset - block_start) as usize
            } else {
                0
            };
            let max_in_block = block_bytes.len().saturating_sub(in_block_start);
            let remaining = buf.len() - written;
            let to_copy = remaining.min(max_in_block);
            buf[written..written + to_copy]
                .copy_from_slice(&block_bytes[in_block_start..in_block_start + to_copy]);
            written += to_copy;
        }

        self.maybe_prefetch(offset, buf.len() as u64).await;
        Ok(())
    }

    /// Read `buf.len()` bytes starting at sector `lba`.
    pub async fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), StreamingDiskError> {
        if buf.len() as u64 % DEFAULT_SECTOR_SIZE != 0 {
            return Err(StreamingDiskError::UnexpectedHttpResponse {
                status: 400,
                reason: "read_sectors buffer length must be multiple of 512".to_string(),
            });
        }
        self.read_at(lba * DEFAULT_SECTOR_SIZE, buf).await
    }

    async fn get_block(&self, block_index: u64) -> Result<Vec<u8>, StreamingDiskError> {
        // Fast path: cached.
        if let Some(bytes) = self.cache.lock().await.read_block(block_index).await? {
            return Ok(bytes);
        }

        let range = {
            let cache = self.cache.lock().await;
            cache.block_range(block_index)
        };

        let bytes = fetch_http_range(&self.client, &self.url, range.start, range.end).await?;
        {
            let mut cache = self.cache.lock().await;
            cache.write_block(block_index, &bytes).await?;
        }
        Ok(bytes)
    }

    async fn maybe_prefetch(&self, offset: u64, len: u64) {
        if len == 0 {
            let mut last = self.last_read_end.lock().await;
            *last = Some(offset);
            return;
        }

        if !self.prefetch.enabled {
            let mut last = self.last_read_end.lock().await;
            *last = Some(offset + len);
            return;
        }

        let mut last = self.last_read_end.lock().await;
        let sequential = last.map(|end| end == offset).unwrap_or(false);
        *last = Some(offset + len);
        drop(last);

        if !sequential {
            return;
        }

        let next_offset = offset + len;
        let next_block = next_offset / self.block_size;
        let distance = self.prefetch.sequential_distance_blocks;
        for i in 0..distance {
            let block = next_block + i;
            if block * self.block_size >= self.total_size {
                break;
            }

            // Best-effort: ignore prefetch errors.
            let _ = self.get_block(block).await;
        }
    }
}

async fn probe_range_support(
    client: &Client<hyper_rustls::HttpsConnector<HttpConnector>>,
    url: &Url,
) -> Result<(u64, bool), StreamingDiskError> {
    let uri: Uri = url.as_str().parse().map_err(|_| {
        StreamingDiskError::UnexpectedHttpResponse {
            status: 0,
            reason: format!("invalid URI: {}", url),
        }
    })?;

    // HEAD probe for size and Accept-Ranges.
    let head = Request::builder()
        .method(Method::HEAD)
        .uri(uri.clone())
        .body(Body::empty())
        .expect("valid request");

    let resp = client.request(head).await?;
    if !resp.status().is_success() {
        return Err(StreamingDiskError::UnexpectedHttpResponse {
            status: resp.status().as_u16(),
            reason: format!("HEAD {}", resp.status()),
        });
    }

    let total_size = resp
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(StreamingDiskError::MissingContentLength)?;

    let accept_ranges = resp
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept_ranges.to_ascii_lowercase().contains("bytes") {
        return Ok((total_size, true));
    }

    // Some servers omit Accept-Ranges but still honor Range. Probe with a small
    // request and look for 206 + Content-Range.
    let range_get = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(RANGE, "bytes=0-0")
        .body(Body::empty())
        .expect("valid request");

    let resp = client.request(range_get).await?;
    if resp.status() != StatusCode::PARTIAL_CONTENT {
        return Ok((total_size, false));
    }

    let content_range = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    parse_content_range(content_range, Some(total_size)).map(|_| (total_size, true))
}

fn block_span_for_range(offset: u64, len: u64, block_size: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let start_block = offset / block_size;
    let end_block = (offset + len - 1) / block_size;
    Some((start_block, end_block))
}

async fn fetch_http_range(
    client: &Client<hyper_rustls::HttpsConnector<HttpConnector>>,
    url: &Url,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, StreamingDiskError> {
    let uri: Uri = url.as_str().parse().map_err(|_| {
        StreamingDiskError::UnexpectedHttpResponse {
            status: 0,
            reason: format!("invalid URI: {}", url),
        }
    })?;

    // Range is inclusive in HTTP.
    let header_value = format!("bytes={}-{}", start, end - 1);
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(RANGE, header_value)
        .body(Body::empty())
        .expect("valid request");

    let mut resp = client.request(req).await?;

    if resp.status() != StatusCode::PARTIAL_CONTENT {
        return Err(StreamingDiskError::UnexpectedHttpResponse {
            status: resp.status().as_u16(),
            reason: format!("expected 206 Partial Content, got {}", resp.status()),
        });
    }

    let content_range = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (got_start, got_end) = parse_content_range(content_range, None)?;
    if got_start != start || got_end != end {
        return Err(StreamingDiskError::InvalidContentRange(content_range.to_string()));
    }

    let mut out = Vec::with_capacity((end - start) as usize);
    while let Some(chunk) = resp.body_mut().data().await {
        let chunk: Bytes = chunk?;
        out.extend_from_slice(&chunk);
    }

    let expected_len = (end - start) as usize;
    if out.len() != expected_len {
        return Err(StreamingDiskError::UnexpectedRangeLength {
            expected: expected_len,
            actual: out.len(),
        });
    }

    Ok(out)
}

fn parse_content_range(
    content_range: &str,
    expected_total_size: Option<u64>,
) -> Result<(u64, u64), StreamingDiskError> {
    // Example: "bytes 0-0/12345"
    let content_range = content_range.trim();
    let Some(rest) = content_range.strip_prefix("bytes ") else {
        return Err(StreamingDiskError::InvalidContentRange(content_range.to_string()));
    };
    let mut parts = rest.split('/');
    let Some(range_part) = parts.next() else {
        return Err(StreamingDiskError::InvalidContentRange(content_range.to_string()));
    };
    let total_part = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return Err(StreamingDiskError::InvalidContentRange(content_range.to_string()));
    }

    let mut range_parts = range_part.split('-');
    let start: u64 = range_parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| StreamingDiskError::InvalidContentRange(content_range.to_string()))?;
    let end_inclusive: u64 = range_parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| StreamingDiskError::InvalidContentRange(content_range.to_string()))?;
    if range_parts.next().is_some() {
        return Err(StreamingDiskError::InvalidContentRange(content_range.to_string()));
    }
    let end = end_inclusive
        .checked_add(1)
        .ok_or_else(|| StreamingDiskError::InvalidContentRange(content_range.to_string()))?;

    if let Some(expected) = expected_total_size {
        let total: u64 = total_part
            .parse()
            .map_err(|_| StreamingDiskError::InvalidContentRange(content_range.to_string()))?;
        if total != expected {
            return Err(StreamingDiskError::InvalidContentRange(content_range.to_string()));
        }
    }

    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_range_ok() {
        assert_eq!(
            parse_content_range("bytes 0-0/10", Some(10)).unwrap(),
            (0, 1)
        );
        assert_eq!(
            parse_content_range("bytes 100-199/1000", None).unwrap(),
            (100, 200)
        );
    }

    #[test]
    fn parse_content_range_rejects_mismatch_total() {
        assert!(parse_content_range("bytes 0-0/11", Some(10)).is_err());
    }

    #[test]
    fn block_span_for_range_handles_boundaries() {
        assert_eq!(block_span_for_range(0, 0, 1024), None);
        assert_eq!(block_span_for_range(0, 1, 1024), Some((0, 0)));
        assert_eq!(block_span_for_range(1023, 1, 1024), Some((0, 0)));
        assert_eq!(block_span_for_range(1023, 2, 1024), Some((0, 1)));
        assert_eq!(block_span_for_range(1024, 1024, 1024), Some((1, 1)));
    }
}
