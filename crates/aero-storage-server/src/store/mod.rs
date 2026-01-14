mod local_fs;
mod manifest;

use std::pin::Pin;
use std::time::SystemTime;

use tokio::io::AsyncRead;

pub use local_fs::LocalFsImageStore;
pub use manifest::{Manifest, ManifestError, ManifestImage};

pub const CONTENT_TYPE_DISK_IMAGE: &str = "application/octet-stream";
pub const CONTENT_TYPE_JSON: &str = "application/json";
pub const MAX_IMAGE_ID_LEN: usize = 128;
/// Maximum length allowed for ETag values supplied by store backends/manifests.
///
/// This is a defensive bound to avoid large allocations / processing if a store backend returns
/// attacker-controlled data.
pub const MAX_ETAG_LEN: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMeta {
    pub size: u64,
    pub etag: Option<String>,
    pub last_modified: Option<SystemTime>,
    pub content_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageCatalogEntry {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub recommended_chunk_size_bytes: Option<u64>,
    pub public: bool,
    pub meta: ImageMeta,
}

pub type BoxedAsyncRead = Pin<Box<dyn AsyncRead + Send>>;

/// A chunked-image object (manifest or chunk file) returned by [`ImageStore`].
///
/// This is a minimal container so store backends can provide a streamable reader plus metadata
/// needed for HTTP headers (size, validators, etc).
pub struct ChunkedObject {
    pub meta: ImageMeta,
    pub reader: BoxedAsyncRead,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid image id: {image_id:?}")]
    InvalidImageId { image_id: String },
    #[error("image not found")]
    NotFound,
    #[error("invalid byte range start={start} len={len} for image of size {size}")]
    InvalidRange { start: u64, len: u64, size: u64 },
    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[async_trait::async_trait]
/// Disk image store abstraction used by `aero-storage-server`.
///
/// This trait is server-side (async) and is not intended to be used as a general-purpose disk
/// abstraction for the emulator/device stack. For in-process synchronous disk image formats and
/// controller/device integration, prefer `aero_storage::{StorageBackend, VirtualDisk}`.
///
/// See `docs/20-storage-trait-consolidation.md`.
pub trait ImageStore: Send + Sync {
    /// List available images (control-plane catalog).
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError>;

    /// Fetch catalog information for a single image.
    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError>;

    /// Fetch the `public` flag for a single image.
    ///
    /// This is used by chunked disk image endpoints to determine cacheability without requiring
    /// access to the underlying raw image file.
    async fn get_image_public(&self, image_id: &str) -> Result<bool, StoreError> {
        Ok(self.get_image(image_id).await?.public)
    }

    /// Fetch core byte-stream metadata for a single image.
    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError>;

    /// Open a range reader for a single image (data-plane).
    async fn open_range(
        &self,
        image_id: &str,
        start: u64,
        len: u64,
    ) -> Result<BoxedAsyncRead, StoreError>;

    /// Open the chunked-disk-image manifest (`manifest.json`) for an image, if available.
    ///
    /// Stores that do not support chunked delivery should return `StoreError::NotFound` (the
    /// default implementation does this).
    async fn open_chunked_manifest(&self, _image_id: &str) -> Result<ChunkedObject, StoreError> {
        Err(StoreError::NotFound)
    }

    /// Open a single chunk object (`chunks/<name>`) for an image, if available.
    ///
    /// Stores that do not support chunked delivery should return `StoreError::NotFound` (the
    /// default implementation does this).
    async fn open_chunked_chunk(
        &self,
        _image_id: &str,
        _chunk_name: &str,
    ) -> Result<ChunkedObject, StoreError> {
        Err(StoreError::NotFound)
    }

    /// Open the chunked-disk-image manifest for a specific version, if available.
    ///
    /// Stores that do not support chunked delivery should return `StoreError::NotFound` (the
    /// default implementation does this).
    async fn open_chunked_manifest_version(
        &self,
        _image_id: &str,
        _version: &str,
    ) -> Result<ChunkedObject, StoreError> {
        Err(StoreError::NotFound)
    }

    /// Open a single chunk object for a specific version, if available.
    ///
    /// Stores that do not support chunked delivery should return `StoreError::NotFound` (the
    /// default implementation does this).
    async fn open_chunked_chunk_version(
        &self,
        _image_id: &str,
        _version: &str,
        _chunk_name: &str,
    ) -> Result<ChunkedObject, StoreError> {
        Err(StoreError::NotFound)
    }

    async fn exists(&self, image_id: &str) -> Result<bool, StoreError> {
        match self.get_meta(image_id).await {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound) => Ok(false),
            Err(err) => Err(err),
        }
    }
}

pub(crate) fn validate_image_id(image_id: &str) -> Result<(), StoreError> {
    if image_id.is_empty()
        || image_id.len() > MAX_IMAGE_ID_LEN
        || image_id == "."
        || image_id == ".."
    {
        return Err(invalid_image_id_error(image_id));
    }

    // Treat `image_id` as an opaque identifier and restrict it to ASCII
    // `[A-Za-z0-9._-]` to prevent path traversal.
    let is_allowed = image_id.bytes().all(|b| {
        matches!(
            b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'
        )
    });

    if !is_allowed {
        return Err(invalid_image_id_error(image_id));
    }

    Ok(())
}

fn invalid_image_id_error(image_id: &str) -> StoreError {
    StoreError::InvalidImageId {
        // `image_id` may come directly from a URL path segment; avoid allocating and propagating
        // an unbounded amount of attacker-controlled data in error strings / logs.
        image_id: truncate_for_error(image_id, MAX_IMAGE_ID_LEN),
    }
}

fn truncate_for_error(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }

    const ELLIPSIS: &str = "...";
    if max_len <= ELLIPSIS.len() {
        return ELLIPSIS[..max_len].to_string();
    }

    // Truncate at a valid UTF-8 boundary.
    let mut end = max_len - ELLIPSIS.len();
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    let mut out = value[..end].to_string();
    out.push_str(ELLIPSIS);
    out
}
