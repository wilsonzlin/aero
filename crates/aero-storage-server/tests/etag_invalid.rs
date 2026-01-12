#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    store::{BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, LocalFsImageStore, StoreError},
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
struct InvalidEtagStore {
    inner: LocalFsImageStore,
}

impl InvalidEtagStore {
    fn corrupt_etag(meta: &mut ImageMeta) {
        meta.etag = Some("bad\nvalue".to_string());
    }

    fn corrupt_entry(entry: &mut ImageCatalogEntry) {
        Self::corrupt_etag(&mut entry.meta);
    }
}

#[async_trait::async_trait]
impl ImageStore for InvalidEtagStore {
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        let mut images = self.inner.list_images().await?;
        for image in &mut images {
            Self::corrupt_entry(image);
        }
        Ok(images)
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        let mut image = self.inner.get_image(image_id).await?;
        Self::corrupt_entry(&mut image);
        Ok(image)
    }

    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        let mut meta = self.inner.get_meta(image_id).await?;
        Self::corrupt_etag(&mut meta);
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

    let store = Arc::new(InvalidEtagStore {
        inner: LocalFsImageStore::new(dir.path()),
    });
    let state = AppState::new(store);
    (aero_storage_server::app(state), dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_image_data_with_invalid_etag_does_not_panic_and_uses_fallback_etag() {
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
    assert!(etag.starts_with("W/\"") && etag.ends_with('\"'));
    assert!(!etag.contains('\n'));

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");

    // Revalidation should work with the fallback ETag.
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
async fn get_image_meta_with_invalid_etag_does_not_panic_and_uses_fallback_etag() {
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
    assert!(etag.starts_with("W/\"") && etag.ends_with('\"'));
    assert!(!etag.contains('\n'));

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["etag"].as_str().unwrap(), etag);

    // Revalidation should work with the fallback ETag.
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
async fn unquoted_store_etag_is_sanitized_to_fallback_etag() {
    #[derive(Clone)]
    struct UnquotedEtagStore {
        inner: LocalFsImageStore,
    }

    #[async_trait::async_trait]
    impl ImageStore for UnquotedEtagStore {
        async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
            self.inner.list_images().await
        }

        async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
            let mut image = self.inner.get_image(image_id).await?;
            image.meta.etag = Some("unquoted".to_string());
            Ok(image)
        }

        async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
            let mut meta = self.inner.get_meta(image_id).await?;
            meta.etag = Some("unquoted".to_string());
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

    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(UnquotedEtagStore {
        inner: LocalFsImageStore::new(dir.path()),
    });
    let state = AppState::new(store);
    let app = aero_storage_server::app(state);

    // Bytes endpoint
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
    assert_ne!(etag, "unquoted");
    assert!(etag.starts_with("W/\"") && etag.ends_with('\"'));

    // Metadata endpoint
    let res = app
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
    let meta_etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_ne!(meta_etag, "unquoted");
    assert!(meta_etag.starts_with("W/\"") && meta_etag.ends_with('\"'));

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["etag"].as_str().unwrap(), meta_etag);
}
