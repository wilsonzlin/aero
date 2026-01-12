#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    http::cache,
    store::{BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, StoreError, CONTENT_TYPE_DISK_IMAGE},
    AppState,
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::{
    io::Cursor,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

#[derive(Clone)]
struct InvalidEtagStore {
    id: String,
    data: Arc<Vec<u8>>,
    etag: String,
    last_modified: Option<SystemTime>,
}

impl InvalidEtagStore {
    fn new(
        id: impl Into<String>,
        data: impl Into<Vec<u8>>,
        etag: impl Into<String>,
        last_modified: Option<SystemTime>,
    ) -> Self {
        Self {
            id: id.into(),
            data: Arc::new(data.into()),
            etag: etag.into(),
            last_modified,
        }
    }

    fn meta(&self) -> ImageMeta {
        ImageMeta {
            size: self.data.len() as u64,
            etag: Some(self.etag.clone()),
            last_modified: self.last_modified,
            content_type: CONTENT_TYPE_DISK_IMAGE,
        }
    }

    fn image_entry(&self) -> ImageCatalogEntry {
        ImageCatalogEntry {
            id: self.id.clone(),
            name: self.id.clone(),
            description: None,
            recommended_chunk_size_bytes: None,
            public: true,
            meta: self.meta(),
        }
    }

    fn ensure_id<'a>(&'a self, image_id: &'a str) -> Result<(), StoreError> {
        if image_id == self.id {
            Ok(())
        } else {
            Err(StoreError::NotFound)
        }
    }
}

#[async_trait::async_trait]
impl ImageStore for InvalidEtagStore {
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        Ok(vec![self.image_entry()])
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        self.ensure_id(image_id)?;
        Ok(self.image_entry())
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
        let start_u64 = start;
        let Some(end) = start.checked_add(len) else {
            return Err(StoreError::InvalidRange { start, len, size });
        };
        if end > size {
            return Err(StoreError::InvalidRange { start, len, size });
        }

        let start =
            usize::try_from(start_u64).map_err(|_| StoreError::InvalidRange { start: start_u64, len, size })?;
        let end =
            usize::try_from(end).map_err(|_| StoreError::InvalidRange { start: start_u64, len, size })?;
        Ok(Box::pin(Cursor::new(self.data[start..end].to_vec())))
    }
}

fn setup_app(store: Arc<dyn ImageStore>) -> axum::Router {
    aero_storage_server::app(AppState::new(store))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_store_etag_with_newline_falls_back_for_bytes_endpoint_and_allows_304() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(1_000) + Duration::from_nanos(456);
    let store = Arc::new(InvalidEtagStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        "\"bad\netag\"",
        Some(last_modified),
    ));
    let app = setup_app(store);

    let res = app
        .clone()
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

    let expected = cache::weak_etag_from_size_and_mtime(13, Some(last_modified));
    assert_eq!(res.headers()[header::ETAG].to_str().unwrap(), expected);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/data")
                .header(header::IF_NONE_MATCH, expected)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_unquoted_store_etag_falls_back_for_meta_endpoint_and_json_body() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(2_000);
    let store = Arc::new(InvalidEtagStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        "not-a-quoted-entity-tag",
        Some(last_modified),
    ));
    let app = setup_app(store);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/meta")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    let expected = cache::weak_etag_from_size_and_mtime(13, Some(last_modified));
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(etag, expected);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(json["etag"].as_str().unwrap(), expected);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/meta")
                .header(header::IF_NONE_MATCH, expected)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_ascii_store_etag_falls_back_for_bytes_endpoint_and_allows_304() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(3_000);
    let store = Arc::new(InvalidEtagStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        "\"é\"",
        Some(last_modified),
    ));
    let app = setup_app(store);

    let res = app
        .clone()
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

    let expected = cache::weak_etag_from_size_and_mtime(13, Some(last_modified));
    assert_eq!(res.headers()[header::ETAG].to_str().unwrap(), expected);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/data")
                .header(header::IF_NONE_MATCH, expected)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_store_etag_falls_back_for_bytes_endpoint() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(4_000);
    let oversized = format!("\"{}\"", "a".repeat(2000));
    let store = Arc::new(InvalidEtagStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        oversized,
        Some(last_modified),
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
    let expected = cache::weak_etag_from_size_and_mtime(13, Some(last_modified));
    assert_eq!(res.headers()[header::ETAG].to_str().unwrap(), expected);
}
