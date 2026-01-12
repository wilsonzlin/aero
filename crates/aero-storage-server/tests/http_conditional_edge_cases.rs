#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    http::{
        images::{router_with_state, ImagesState},
        range::RangeOptions,
    },
    metrics::Metrics,
    store::{
        BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, StoreError,
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
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use tower::ServiceExt;

#[derive(Clone)]
struct FixedImageStore {
    id: String,
    data: Arc<Vec<u8>>,
    meta: ImageMeta,
}

impl FixedImageStore {
    fn new(id: impl Into<String>, data: impl Into<Vec<u8>>, mut meta: ImageMeta) -> Self {
        let data = data.into();
        meta.size = data.len() as u64;
        Self {
            id: id.into(),
            data: Arc::new(data),
            meta,
        }
    }

    fn image_entry(&self) -> ImageCatalogEntry {
        ImageCatalogEntry {
            id: self.id.clone(),
            name: self.id.clone(),
            description: None,
            recommended_chunk_size_bytes: None,
            public: true,
            meta: self.meta.clone(),
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
impl ImageStore for FixedImageStore {
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        Ok(vec![self.image_entry()])
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        self.ensure_id(image_id)?;
        Ok(self.image_entry())
    }

    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        self.ensure_id(image_id)?;
        Ok(self.meta.clone())
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
            return Err(StoreError::InvalidRange {
                start: start_u64,
                len,
                size,
            });
        };
        if end > size {
            return Err(StoreError::InvalidRange {
                start: start_u64,
                len,
                size,
            });
        }

        let start = usize::try_from(start_u64).map_err(|_| StoreError::InvalidRange {
            start: start_u64,
            len,
            size,
        })?;
        let end = usize::try_from(end).map_err(|_| StoreError::InvalidRange {
            start: start_u64,
            len,
            size,
        })?;
        let slice = self.data[start..end].to_vec();
        Ok(Box::pin(Cursor::new(slice)))
    }
}

fn setup_app(store: Arc<dyn ImageStore>) -> axum::Router {
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_range_options(RangeOptions {
        max_total_bytes: 1024,
    });
    router_with_state(state)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn if_range_http_date_at_or_after_last_modified_allows_range() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let if_range = httpdate::fmt_http_date(last_modified + Duration::from_secs(60));

    let store = Arc::new(FixedImageStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        ImageMeta {
            size: 0,
            etag: None,
            last_modified: Some(last_modified),
            content_type: CONTENT_TYPE_DISK_IMAGE,
        },
    ));
    let app = setup_app(store);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
                .header(header::IF_RANGE, if_range)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes 0-0/13"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"H");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn if_range_http_date_ignores_subsecond_precision() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(1_000_000) + Duration::from_nanos(456);
    let if_range = httpdate::fmt_http_date(last_modified);

    let store = Arc::new(FixedImageStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        ImageMeta {
            size: 0,
            etag: None,
            last_modified: Some(last_modified),
            content_type: CONTENT_TYPE_DISK_IMAGE,
        },
    ));
    let app = setup_app(store);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
                .header(header::IF_RANGE, if_range)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"H");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn if_range_http_date_before_last_modified_ignores_range() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let if_range = httpdate::fmt_http_date(last_modified - Duration::from_secs(60));

    let store = Arc::new(FixedImageStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        ImageMeta {
            size: 0,
            etag: None,
            last_modified: Some(last_modified),
            content_type: CONTENT_TYPE_DISK_IMAGE,
        },
    ));
    let app = setup_app(store);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
                .header(header::IF_RANGE, if_range)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        !res.headers().contains_key(header::CONTENT_RANGE),
        "Range should be ignored when If-Range validator does not match"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn if_none_match_takes_precedence_over_if_modified_since() {
    let last_modified = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let earlier = httpdate::fmt_http_date(last_modified - Duration::from_secs(60));

    let store = Arc::new(FixedImageStore::new(
        "test.img",
        b"Hello, world!".to_vec(),
        ImageMeta {
            size: 0,
            etag: Some("\"etag\"".to_string()),
            last_modified: Some(last_modified),
            content_type: CONTENT_TYPE_DISK_IMAGE,
        },
    ));
    let app = setup_app(store);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::IF_NONE_MATCH, "\"etag\"")
                .header(header::IF_MODIFIED_SINCE, earlier)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}
