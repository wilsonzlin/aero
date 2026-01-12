#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    http::images::{router_with_state, ImagesState},
    metrics::Metrics,
    store::{
        BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, LocalFsImageStore, StoreError,
        CONTENT_TYPE_DISK_IMAGE,
    },
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::UNIX_EPOCH,
};
use tempfile::tempdir;
use tower::ServiceExt;

async fn setup_app_with_fs_store(require_range: bool) -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_require_range(require_range);
    (router_with_state(state), dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_range_rejects_unsupported_range_unit() {
    let (app, _dir) = setup_app_with_fs_store(true).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "items=0-0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes */13"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[derive(Clone)]
struct CountingStore {
    image_id: String,
    meta: ImageMeta,
    data: Arc<Vec<u8>>,
    open_range_calls: Arc<AtomicUsize>,
}

impl CountingStore {
    fn new(image_id: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        let data = data.into();
        Self {
            image_id: image_id.into(),
            meta: ImageMeta {
                size: data.len() as u64,
                etag: Some("\"etag\"".to_string()),
                last_modified: Some(UNIX_EPOCH),
                content_type: CONTENT_TYPE_DISK_IMAGE,
            },
            data: Arc::new(data),
            open_range_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn entry(&self) -> ImageCatalogEntry {
        ImageCatalogEntry {
            id: self.image_id.clone(),
            name: self.image_id.clone(),
            description: None,
            recommended_chunk_size_bytes: None,
            public: true,
            meta: self.meta.clone(),
        }
    }
}

#[async_trait::async_trait]
impl ImageStore for CountingStore {
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        Ok(vec![self.entry()])
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        if image_id != self.image_id {
            return Err(StoreError::NotFound);
        }
        Ok(self.entry())
    }

    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        if image_id != self.image_id {
            return Err(StoreError::NotFound);
        }
        Ok(self.meta.clone())
    }

    async fn open_range(
        &self,
        image_id: &str,
        start: u64,
        len: u64,
    ) -> Result<BoxedAsyncRead, StoreError> {
        if image_id != self.image_id {
            return Err(StoreError::NotFound);
        }

        self.open_range_calls.fetch_add(1, Ordering::SeqCst);

        let start = usize::try_from(start).map_err(|_| StoreError::InvalidRange {
            start,
            len,
            size: self.meta.size,
        })?;
        let end = start
            .checked_add(usize::try_from(len).map_err(|_| StoreError::InvalidRange {
                start: start as u64,
                len,
                size: self.meta.size,
            })?)
            .ok_or(StoreError::InvalidRange {
                start: start as u64,
                len,
                size: self.meta.size,
            })?;

        let slice = self
            .data
            .get(start..end)
            .ok_or(StoreError::InvalidRange {
                start: start as u64,
                len,
                size: self.meta.size,
            })?
            .to_vec();

        Ok(Box::pin(Cursor::new(slice)))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_range_if_range_mismatch_returns_412_and_does_not_open_range() {
    let store = Arc::new(CountingStore::new("test.img", b"Hello, world!"));
    let open_range_calls = store.open_range_calls.clone();
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_require_range(true);
    let app = router_with_state(state);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
                .header(header::IF_RANGE, "\"mismatch\"")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PRECONDITION_FAILED);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());

    assert_eq!(open_range_calls.load(Ordering::SeqCst), 0);
}
