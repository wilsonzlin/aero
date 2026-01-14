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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_cors_does_not_vary_on_origin_for_bytes_get_and_head() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let chunk_root = dir.path().join("chunked").join("test.img");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");
    tokio::fs::write(
        chunk_root.join("manifest.json"),
        b"{\"schema\":\"aero.chunked-disk-image.v1\"}",
    )
    .await
    .expect("write chunked manifest");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"x")
        .await
        .expect("write chunk");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let app = aero_storage_server::app(AppState::new(store));

    for method in [Method::GET, Method::HEAD] {
        for uri in [
            "/v1/images/test.img",
            "/v1/images/test.img/chunked/manifest.json",
            "/v1/images/test.img/chunked/chunks/00000000.bin",
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(uri)
                        .header(header::ORIGIN, "https://example.com")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(resp.status(), StatusCode::OK, "{method} {uri}");
            assert!(!has_vary_token(resp.headers(), "origin"), "{method} {uri}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_cors_origin_includes_vary_origin_for_bytes() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let chunk_root = dir.path().join("chunked").join("test.img");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");
    tokio::fs::write(
        chunk_root.join("manifest.json"),
        b"{\"schema\":\"aero.chunked-disk-image.v1\"}",
    )
    .await
    .expect("write chunked manifest");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"x")
        .await
        .expect("write chunk");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    for uri in [
        "/v1/images/test.img",
        "/v1/images/test.img/chunked/manifest.json",
        "/v1/images/test.img/chunked/chunks/00000000.bin",
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

        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        assert!(has_vary_token(resp.headers(), "origin"), "{uri}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_includes_origin_and_acr_vary_tokens_when_origin_specific() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let chunk_root = dir.path().join("chunked").join("test.img");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");
    tokio::fs::write(
        chunk_root.join("manifest.json"),
        b"{\"schema\":\"aero.chunked-disk-image.v1\"}",
    )
    .await
    .expect("write chunked manifest");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"x")
        .await
        .expect("write chunk");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    for uri in [
        "/v1/images/test.img",
        "/v1/images/test.img/chunked/manifest.json",
        "/v1/images/test.img/chunked/chunks/00000000.bin",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri(uri)
                    .header(header::ORIGIN, "https://example.com")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "Range")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT, "{uri}");
        assert!(has_vary_token(resp.headers(), "origin"), "{uri}");
        assert!(
            has_vary_token(resp.headers(), "access-control-request-method"),
            "{uri}"
        );
        assert!(
            has_vary_token(resp.headers(), "access-control-request-headers"),
            "{uri}"
        );
    }
}
