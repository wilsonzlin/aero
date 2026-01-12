mod local_fs;
mod manifest;

use std::pin::Pin;
use std::time::SystemTime;

use tokio::io::AsyncRead;

pub use local_fs::LocalFsImageStore;
pub use manifest::{Manifest, ManifestError, ManifestImage};

pub const CONTENT_TYPE_DISK_IMAGE: &str = "application/octet-stream";
pub(crate) const MAX_IMAGE_ID_LEN: usize = 128;

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
pub trait ImageStore: Send + Sync {
    /// List available images (control-plane catalog).
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError>;

    /// Fetch catalog information for a single image.
    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError>;

    /// Fetch core byte-stream metadata for a single image.
    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError>;

    /// Open a range reader for a single image (data-plane).
    async fn open_range(
        &self,
        image_id: &str,
        start: u64,
        len: u64,
    ) -> Result<BoxedAsyncRead, StoreError>;

    async fn exists(&self, image_id: &str) -> Result<bool, StoreError> {
        match self.get_meta(image_id).await {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound) => Ok(false),
            Err(err) => Err(err),
        }
    }
}

pub(crate) fn validate_image_id(image_id: &str) -> Result<(), StoreError> {
    if image_id.is_empty() || image_id.len() > MAX_IMAGE_ID_LEN || image_id == "." || image_id == ".."
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
