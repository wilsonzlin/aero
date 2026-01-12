#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

fn has_vary_token(headers: &axum::http::HeaderMap, token: &str) -> bool {
    let token = token.to_ascii_lowercase();
    headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .any(|t| t == token)
}

#[tokio::test]
async fn public_cors_does_not_vary_on_origin_for_bytes_get_and_head() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let app = aero_storage_server::app(AppState::new(store));

    for method in [Method::GET, Method::HEAD] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/v1/images/test.img")
                    .header(header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!has_vary_token(resp.headers(), "origin"));
    }
}

#[tokio::test]
async fn explicit_cors_origin_includes_vary_origin_for_bytes() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(has_vary_token(resp.headers(), "origin"));
}

#[tokio::test]
async fn preflight_includes_origin_and_acr_vary_tokens_when_origin_specific() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/images/test.img")
                .header(header::ORIGIN, "https://example.com")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "Range")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(has_vary_token(resp.headers(), "origin"));
    assert!(has_vary_token(resp.headers(), "access-control-request-method"));
    assert!(has_vary_token(resp.headers(), "access-control-request-headers"));
}

