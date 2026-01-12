#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    store::{
        BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, LocalFsImageStore, StoreError,
    },
    AppState,
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

#[derive(Clone)]
struct NoEtagStore {
    inner: LocalFsImageStore,
}

#[async_trait::async_trait]
impl ImageStore for NoEtagStore {
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        let mut images = self.inner.list_images().await?;
        for image in &mut images {
            image.meta.etag = None;
        }
        Ok(images)
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        let mut image = self.inner.get_image(image_id).await?;
        image.meta.etag = None;
        Ok(image)
    }

    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        let mut meta = self.inner.get_meta(image_id).await?;
        meta.etag = None;
        Ok(meta)
    }

    async fn open_range(
        &self,
        image_id: &str,
        start: u64,
        len: u64,
    ) -> Result<BoxedAsyncRead, StoreError> {
        self.inner.open_range(image_id, start, len).await
    }
}

async fn setup_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(NoEtagStore {
        inner: LocalFsImageStore::new(dir.path()),
    });
    let state = AppState::new(store);
    (aero_storage_server::app(state), dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_image_meta_with_if_none_match_returns_304() {
    let (app, _dir) = setup_app().await;

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
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/meta")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_image_data_with_if_none_match_returns_304() {
    let (app, _dir) = setup_app().await;

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
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/data")
                .header(header::IF_NONE_MATCH, etag)
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
async fn head_image_data_includes_etag_last_modified_and_accept_ranges() {
    let (app, _dir) = setup_app().await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/test.img/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().contains_key(header::ETAG));
    assert!(res.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        res.headers()[header::ACCEPT_RANGES].to_str().unwrap(),
        "bytes"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}
