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
use http_body_util::BodyExt;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

async fn setup_app(require_range: bool) -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_require_range(require_range);
    (router_with_state(state), dir)
}

#[tokio::test]
async fn require_range_rejects_get_without_range_header() {
    let (app, _dir) = setup_app(true).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
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
    assert_eq!(
        res.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        res.headers()["access-control-allow-origin"].to_str().unwrap(),
        "*"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test]
async fn require_range_allows_valid_range_requests() {
    let (app, _dir) = setup_app(true).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
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

