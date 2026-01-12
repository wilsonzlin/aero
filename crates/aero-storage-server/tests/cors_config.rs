#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn cors_origin_override_is_applied_to_metadata_and_bytes_endpoints() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    for (name, uri) in [
        ("bytes", "/v1/images/test.img"),
        ("meta", "/v1/images/test.img/meta"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK, "{name}");
        assert_eq!(
            resp.headers()["access-control-allow-origin"]
                .to_str()
                .unwrap(),
            "https://example.com",
            "{name}"
        );
        assert_eq!(
            resp.headers()["access-control-allow-credentials"]
                .to_str()
                .unwrap(),
            "true",
            "{name}"
        );
    }
}

#[tokio::test]
async fn cors_preflight_for_metadata_endpoints_allows_if_none_match() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    for (name, uri) in [("list", "/v1/images"), ("meta", "/v1/images/test.img/meta")] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri(uri)
                    .header(header::ORIGIN, "https://example.com")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "if-none-match")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT, "{name}");
        assert_eq!(
            resp.headers()["access-control-allow-origin"]
                .to_str()
                .unwrap(),
            "https://example.com",
            "{name}"
        );
        assert_eq!(
            resp.headers()["access-control-allow-credentials"]
                .to_str()
                .unwrap(),
            "true",
            "{name}"
        );
        assert!(resp.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("if-none-match"));
        assert_eq!(
            resp.headers()["access-control-allow-methods"]
                .to_str()
                .unwrap(),
            "GET, HEAD, OPTIONS",
            "{name}"
        );
    }
}

#[tokio::test]
async fn cors_headers_are_present_on_rejected_pathological_image_ids() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    let long_raw = "a".repeat(aero_storage_server::store::MAX_IMAGE_ID_LEN * 3 + 1);

    // Bytes endpoint guard should return 404 but still include CORS headers so callers don't see
    // a generic CORS failure.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/images/{long_raw}"))
                .header(header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "https://example.com"
    );
    assert_eq!(
        resp.headers()["access-control-allow-credentials"]
            .to_str()
            .unwrap(),
        "true"
    );

    // Metadata endpoint guard should return 400 and also include CORS headers.
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/images/{long_raw}/meta"))
                .header(header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "https://example.com"
    );
    assert_eq!(
        resp.headers()["access-control-allow-credentials"]
            .to_str()
            .unwrap(),
        "true"
    );
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-cache"
    );
}
