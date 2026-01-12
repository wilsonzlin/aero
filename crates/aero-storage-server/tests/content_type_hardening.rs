#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    store::{BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, StoreError, CONTENT_TYPE_DISK_IMAGE},
    AppState,
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::{io::Cursor, sync::Arc, time::SystemTime};
use tower::ServiceExt;

#[derive(Clone)]
struct InvalidContentTypeStore {
    id: String,
    data: Arc<Vec<u8>>,
    content_type: &'static str,
}

impl InvalidContentTypeStore {
    fn new(id: impl Into<String>, data: impl Into<Vec<u8>>, content_type: &'static str) -> Self {
        Self {
            id: id.into(),
            data: Arc::new(data.into()),
            content_type,
        }
    }

    fn meta(&self) -> ImageMeta {
        ImageMeta {
            size: self.data.len() as u64,
            etag: Some("\"ok\"".to_string()),
            last_modified: Some(SystemTime::UNIX_EPOCH),
            content_type: self.content_type,
        }
    }

    fn entry(&self) -> ImageCatalogEntry {
        ImageCatalogEntry {
            id: self.id.clone(),
            name: self.id.clone(),
            description: None,
            recommended_chunk_size_bytes: None,
            public: true,
            meta: self.meta(),
        }
    }

    fn ensure_id(&self, image_id: &str) -> Result<(), StoreError> {
        if image_id == self.id {
            Ok(())
        } else {
            Err(StoreError::NotFound)
        }
    }
}

#[async_trait::async_trait]
impl ImageStore for InvalidContentTypeStore {
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        Ok(vec![self.entry()])
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        self.ensure_id(image_id)?;
        Ok(self.entry())
    }

    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        self.ensure_id(image_id)?;
        Ok(self.meta())
    }

    async fn open_range(
        &self,
        image_id: &str,
        start: u64,
        len: u64,
    ) -> Result<BoxedAsyncRead, StoreError> {
        self.ensure_id(image_id)?;
        let size = self.data.len() as u64;
        let Some(end) = start.checked_add(len) else {
            return Err(StoreError::InvalidRange { start, len, size });
        };
        if end > size {
            return Err(StoreError::InvalidRange { start, len, size });
        }
        let start_usize =
            usize::try_from(start).map_err(|_| StoreError::InvalidRange { start, len, size })?;
        let end_usize =
            usize::try_from(end).map_err(|_| StoreError::InvalidRange { start, len, size })?;
        Ok(Box::pin(Cursor::new(
            self.data[start_usize..end_usize].to_vec(),
        )))
    }
}

fn setup_app(store: Arc<dyn ImageStore>) -> axum::Router {
    aero_storage_server::app(AppState::new(store))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_store_content_type_does_not_panic_and_uses_fallback_content_type() {
    let store = Arc::new(InvalidContentTypeStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        "bad\nvalue",
    ));
    let app = setup_app(store);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        CONTENT_TYPE_DISK_IMAGE
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");
}

