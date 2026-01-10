mod local_fs;

use std::pin::Pin;
use std::time::SystemTime;

use tokio::io::AsyncRead;

pub use local_fs::LocalFsImageStore;

pub const CONTENT_TYPE_DISK_IMAGE: &str = "application/octet-stream";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMeta {
    pub size: u64,
    pub etag: Option<String>,
    pub last_modified: Option<SystemTime>,
    pub content_type: &'static str,
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[async_trait::async_trait]
pub trait ImageStore: Send + Sync {
    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError>;

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
