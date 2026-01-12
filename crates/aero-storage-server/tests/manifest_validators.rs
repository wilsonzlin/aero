#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

async fn setup_app_with_manifest() -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("disk.img"), b"Hello from disk!")
        .await
        .expect("write image");

    tokio::fs::write(
        dir.path().join("manifest.json"),
        r#"{
  "images": [
    {
      "id": "disk",
      "file": "disk.img",
      "name": "Disk",
      "public": true,
      "etag": "\"stable-etag-v1\"",
      "last_modified": "2026-01-10T00:00:00Z"
    }
  ]
}"#,
    )
    .await
    .expect("write manifest");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store);
    (aero_storage_server::app(state), dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_provided_etag_and_last_modified_are_used_for_bytes_endpoint() {
    let (app, _dir) = setup_app_with_manifest().await;

    // HEAD includes the manifest-provided validators.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/disk")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(etag, "\"stable-etag-v1\"");
    assert_eq!(
        res.headers()[header::LAST_MODIFIED].to_str().unwrap(),
        "Sat, 10 Jan 2026 00:00:00 GMT"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());

    // Conditional GET uses the manifest-provided ETag and returns 304.
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/disk")
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
async fn manifest_provided_etag_and_last_modified_are_used_for_meta_endpoint() {
    let (app, _dir) = setup_app_with_manifest().await;

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/disk/meta")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(etag, "\"stable-etag-v1\"");
    assert_eq!(
        res.headers()[header::LAST_MODIFIED].to_str().unwrap(),
        "Sat, 10 Jan 2026 00:00:00 GMT"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());

    // Conditional GET uses the manifest-provided ETag and returns 304.
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/disk/meta")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_last_modified_drives_if_modified_since_for_bytes_endpoint() {
    let (app, _dir) = setup_app_with_manifest().await;

    let head = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/disk")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    let last_modified = head.headers()[header::LAST_MODIFIED].to_str().unwrap().to_string();

    // If-Modified-Since uses 1-second resolution; sending the exact Last-Modified value should
    // return 304.
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/disk")
                .header(header::IF_MODIFIED_SINCE, last_modified)
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
async fn manifest_last_modified_drives_if_modified_since_for_meta_endpoint() {
    let (app, _dir) = setup_app_with_manifest().await;

    let head = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/disk/meta")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    let last_modified = head.headers()[header::LAST_MODIFIED].to_str().unwrap().to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/disk/meta")
                .header(header::IF_MODIFIED_SINCE, last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}
