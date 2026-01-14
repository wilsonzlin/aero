#![cfg(not(target_arch = "wasm32"))]

use std::{sync::Arc, time::Duration};

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_preflight_max_age_is_configurable_for_bytes_and_metadata() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store).with_cors_preflight_max_age(Duration::from_secs(123));
    let app = aero_storage_server::app(state);

    for (name, uri) in [
        ("bytes", "/v1/images/test.img"),
        ("meta", "/v1/images/test.img/meta"),
        ("chunked-manifest", "/v1/images/test.img/chunked/manifest.json"),
        ("chunked-chunk", "/v1/images/test.img/chunked/chunks/00000000.bin"),
        ("chunked-manifest-v1", "/v1/images/test.img/chunked/v1/manifest.json"),
        ("chunked-chunk-v1", "/v1/images/test.img/chunked/v1/chunks/00000000.bin"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri(uri)
                    .header(header::ORIGIN, "https://example.com")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "range")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::NO_CONTENT, "{name}");
        assert_eq!(
            res.headers()["access-control-max-age"].to_str().unwrap(),
            "123",
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_multi_origin_allowlist_echoes_allowed_origin_and_omits_disallowed() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    // Chunked artifacts for the chunked endpoints.
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

    // Versioned chunked artifacts for explicit `/chunked/:version/...` routes.
    let chunk_root_v1 = dir.path().join("chunked").join("test.img").join("v1");
    tokio::fs::create_dir_all(chunk_root_v1.join("chunks"))
        .await
        .expect("create chunk dirs (v1)");
    tokio::fs::write(
        chunk_root_v1.join("manifest.json"),
        b"{\"schema\":\"aero.chunked-disk-image.v1\"}",
    )
    .await
    .expect("write chunked manifest (v1)");
    tokio::fs::write(chunk_root_v1.join("chunks/00000000.bin"), b"x")
        .await
        .expect("write chunk (v1)");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allowed_origins(["https://a.example", "https://b.example"])
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    for (name, uri) in [
        ("bytes", "/v1/images/test.img"),
        ("meta", "/v1/images/test.img/meta"),
        ("chunked-manifest", "/v1/images/test.img/chunked/manifest.json"),
        ("chunked-chunk", "/v1/images/test.img/chunked/chunks/00000000.bin"),
        ("chunked-manifest-v1", "/v1/images/test.img/chunked/v1/manifest.json"),
        ("chunked-chunk-v1", "/v1/images/test.img/chunked/v1/chunks/00000000.bin"),
        ("metrics", "/metrics"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(header::ORIGIN, "https://a.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            res.headers()["access-control-allow-origin"]
                .to_str()
                .unwrap(),
            "https://a.example",
            "{name}"
        );
    }

    for (name, uri) in [
        ("bytes", "/v1/images/test.img"),
        ("meta", "/v1/images/test.img/meta"),
        ("chunked-manifest", "/v1/images/test.img/chunked/manifest.json"),
        ("chunked-chunk", "/v1/images/test.img/chunked/chunks/00000000.bin"),
        ("chunked-manifest-v1", "/v1/images/test.img/chunked/v1/manifest.json"),
        ("chunked-chunk-v1", "/v1/images/test.img/chunked/v1/chunks/00000000.bin"),
        ("metrics", "/metrics"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(header::ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            res.headers().get("access-control-allow-origin").is_none(),
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_range_bytes_enforced_on_both_images_endpoints() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store).with_max_range_bytes(1);
    let app = aero_storage_server::app(state);

    for (name, uri) in [
        ("short", "/v1/images/test.img"),
        ("data", "/v1/images/test.img/data"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(header::RANGE, "bytes=0-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_cache_max_age_is_configurable() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store).with_public_cache_max_age(Duration::from_secs(5));
    let app = aero_storage_server::app(state);

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

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=5, no-transform"
    );
}
