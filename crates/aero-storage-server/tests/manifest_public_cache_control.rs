#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    http::images::{router_with_state, ImagesState},
    metrics::Metrics,
    store::LocalFsImageStore,
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

async fn setup_app_with_private_image() -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("private.img"), b"Hello, world!")
        .await
        .expect("write image bytes");

    let manifest = r#"{
      "images": [
        { "id": "private.img", "file": "private.img", "name": "Private", "public": false }
      ]
    }"#;
    tokio::fs::write(dir.path().join("manifest.json"), manifest)
        .await
        .expect("write manifest");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    (router_with_state(state), dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_manifest_image_without_credentials_is_not_publicly_cacheable() {
    let (app, _dir) = setup_app_with_private_image().await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/private.img")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let cache_control = res.headers()[header::CACHE_CONTROL].to_str().unwrap();
    assert!(
        cache_control.contains("no-store"),
        "expected Cache-Control to contain no-store, got {cache_control:?}"
    );
    assert!(
        !cache_control.contains("public"),
        "expected Cache-Control to not contain public, got {cache_control:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_manifest_image_range_without_credentials_is_not_publicly_cacheable() {
    let (app, _dir) = setup_app_with_private_image().await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/private.img")
                .header(header::RANGE, "bytes=0-0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    let cache_control = res.headers()[header::CACHE_CONTROL].to_str().unwrap();
    assert!(
        cache_control.contains("no-store"),
        "expected Cache-Control to contain no-store, got {cache_control:?}"
    );
    assert!(
        !cache_control.contains("public"),
        "expected Cache-Control to not contain public, got {cache_control:?}"
    );
}
