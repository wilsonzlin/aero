#![cfg(not(target_arch = "wasm32"))]

use aero_storage_server::{
    http::{self, images::ImagesState},
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

const IMAGE_ID: &str = "disk";

async fn setup_app(max_chunk_bytes: Option<u64>) -> (axum::Router, tempfile::TempDir, String) {
    let dir = tempdir().expect("tempdir");

    // Backing image file required by the image catalog.
    tokio::fs::write(dir.path().join("disk.img"), b"raw image bytes")
        .await
        .expect("write disk.img");

    // Image catalog manifest.json so we can control `public` and avoid directory listing fallback.
    let catalog = serde_json::json!({
        "images": [
            { "id": IMAGE_ID, "file": "disk.img", "name": "Disk", "public": true }
        ]
    })
    .to_string();
    tokio::fs::write(dir.path().join("manifest.json"), catalog)
        .await
        .expect("write manifest.json");

    // Chunked artifacts.
    let chunk_root = dir.path().join("chunked").join(IMAGE_ID);
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "imageId": IMAGE_ID,
        "version": "v1",
        "mimeType": "application/octet-stream",
        "totalSize": 4,
        "chunkSize": 2,
        "chunkCount": 2,
        "chunkIndexWidth": 8,
        "chunks": [
            { "size": 2 },
            { "size": 2 }
        ]
    })
    .to_string();
    tokio::fs::write(chunk_root.join("manifest.json"), manifest.as_bytes())
        .await
        .expect("write chunked manifest.json");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"ab")
        .await
        .expect("write chunk0");
    tokio::fs::write(chunk_root.join("chunks/00000001.bin"), b"cd")
        .await
        .expect("write chunk1");

    let store = Arc::new(LocalFsImageStore::new(dir.path()).with_require_manifest(true));
    let metrics = Arc::new(Metrics::new());
    let mut state = ImagesState::new(store, metrics);
    if let Some(max) = max_chunk_bytes {
        state = state.with_max_chunk_bytes(max);
    }

    (http::router_with_state(state), dir, manifest)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_manifest_endpoint_has_expected_headers() {
    let (app, _dir, expected_manifest) = setup_app(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/manifest.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/json"
    );
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), expected_manifest);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_versioned_layout_is_supported_via_catalog_field() {
    let dir = tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("disk.img"), b"raw image bytes")
        .await
        .expect("write disk.img");

    let catalog = serde_json::json!({
        "images": [
            { "id": IMAGE_ID, "file": "disk.img", "name": "Disk", "public": true, "chunked_version": "v1" }
        ]
    })
    .to_string();
    tokio::fs::write(dir.path().join("manifest.json"), catalog)
        .await
        .expect("write manifest.json");

    let chunk_root = dir.path().join("chunked").join(IMAGE_ID).join("v1");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "imageId": IMAGE_ID,
        "version": "v1",
        "mimeType": "application/octet-stream",
        "totalSize": 2,
        "chunkSize": 2,
        "chunkCount": 1,
        "chunkIndexWidth": 8,
        "chunks": [
            { "size": 2 }
        ]
    })
    .to_string();
    tokio::fs::write(chunk_root.join("manifest.json"), manifest.as_bytes())
        .await
        .expect("write chunked manifest.json");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"hi")
        .await
        .expect("write chunk0");

    let store = Arc::new(LocalFsImageStore::new(dir.path()).with_require_manifest(true));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/manifest.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), manifest);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"hi");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_http_endpoints_work() {
    let dir = tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("disk.img"), b"raw image bytes")
        .await
        .expect("write disk.img");

    let catalog = serde_json::json!({
        "images": [
            { "id": IMAGE_ID, "file": "disk.img", "name": "Disk", "public": true }
        ]
    })
    .to_string();
    tokio::fs::write(dir.path().join("manifest.json"), catalog)
        .await
        .expect("write manifest.json");

    let chunk_root = dir.path().join("chunked").join(IMAGE_ID).join("v1");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "imageId": IMAGE_ID,
        "version": "v1",
        "mimeType": "application/octet-stream",
        "totalSize": 2,
        "chunkSize": 2,
        "chunkCount": 1,
        "chunkIndexWidth": 8,
        "chunks": [
            { "size": 2 }
        ]
    })
    .to_string();
    tokio::fs::write(chunk_root.join("manifest.json"), manifest.as_bytes())
        .await
        .expect("write chunked manifest.json");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"hi")
        .await
        .expect("write chunk0");

    let store = Arc::new(LocalFsImageStore::new(dir.path()).with_require_manifest(true));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/manifest.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), manifest);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"hi");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_endpoints_have_expected_headers() {
    let dir = tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("disk.img"), b"raw image bytes")
        .await
        .expect("write disk.img");

    let catalog = serde_json::json!({
        "images": [
            { "id": IMAGE_ID, "file": "disk.img", "name": "Disk", "public": true }
        ]
    })
    .to_string();
    tokio::fs::write(dir.path().join("manifest.json"), catalog)
        .await
        .expect("write manifest.json");

    let chunk_root = dir.path().join("chunked").join(IMAGE_ID).join("v1");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "imageId": IMAGE_ID,
        "version": "v1",
        "mimeType": "application/octet-stream",
        "totalSize": 2,
        "chunkSize": 2,
        "chunkCount": 1,
        "chunkIndexWidth": 8,
        "chunks": [
            { "size": 2 }
        ]
    })
    .to_string();
    tokio::fs::write(chunk_root.join("manifest.json"), manifest.as_bytes())
        .await
        .expect("write chunked manifest.json");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"hi")
        .await
        .expect("write chunk0");

    let store = Arc::new(LocalFsImageStore::new(dir.path()).with_require_manifest(true));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/manifest.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/json"
    );
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), manifest);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/octet-stream"
    );
    assert_eq!(
        resp.headers()[header::CONTENT_ENCODING].to_str().unwrap(),
        "identity"
    );
    assert_eq!(resp.headers()[header::CONTENT_LENGTH].to_str().unwrap(), "2");
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"hi");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_options_preflight_includes_cors_and_corp_headers() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/images/disk/chunked/manifest.json")
                .header(header::ORIGIN, "https://example.com")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-allow-methods"]
            .to_str()
            .unwrap(),
        "GET, HEAD, OPTIONS"
    );
    assert_eq!(
        resp.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap(),
        "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type"
    );
    assert_eq!(
        resp.headers()["access-control-max-age"].to_str().unwrap(),
        "86400"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_options_preflight_includes_cors_and_corp_headers() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/images/disk/chunked/v1/manifest.json")
                .header(header::ORIGIN, "https://example.com")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-allow-methods"]
            .to_str()
            .unwrap(),
        "GET, HEAD, OPTIONS"
    );
    assert_eq!(
        resp.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap(),
        "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type"
    );
    assert_eq!(
        resp.headers()["access-control-max-age"].to_str().unwrap(),
        "86400"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_chunk_endpoint_has_expected_headers_and_body() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/octet-stream"
    );
    assert_eq!(
        resp.headers()[header::CONTENT_ENCODING].to_str().unwrap(),
        "identity"
    );
    assert_eq!(resp.headers()[header::CONTENT_LENGTH].to_str().unwrap(), "2");
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ab");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_chunked_responses_with_cookie_are_not_publicly_cacheable() {
    let (app, _dir, _manifest) = setup_app(None).await;

    for uri in [
        "/v1/images/disk/chunked/manifest.json",
        "/v1/images/disk/chunked/chunks/00000000.bin",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::COOKIE, "a=b")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
            "private, no-store, no-transform",
            "{uri}"
        );
    }

    // Repeat for versioned endpoints using a versioned on-disk layout.
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("disk.img"), b"raw image bytes")
        .await
        .expect("write disk.img");
    let catalog = serde_json::json!({
        "images": [
            { "id": IMAGE_ID, "file": "disk.img", "name": "Disk", "public": true }
        ]
    })
    .to_string();
    tokio::fs::write(dir.path().join("manifest.json"), catalog)
        .await
        .expect("write manifest.json");
    let chunk_root = dir.path().join("chunked").join(IMAGE_ID).join("v1");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");
    tokio::fs::write(
        chunk_root.join("manifest.json"),
        b"{\"schema\":\"aero.chunked-disk-image.v1\"}",
    )
    .await
    .expect("write chunked manifest");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"ab")
        .await
        .expect("write chunk");

    let store = Arc::new(LocalFsImageStore::new(dir.path()).with_require_manifest(true));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    for uri in [
        "/v1/images/disk/chunked/v1/manifest.json",
        "/v1/images/disk/chunked/v1/chunks/00000000.bin",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::COOKIE, "a=b")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
            "private, no-store, no-transform",
            "{uri}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_images_are_not_publicly_cacheable_for_chunked_endpoints() {
    let dir = tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("disk.img"), b"raw image bytes")
        .await
        .expect("write disk.img");

    // Same fixture as the public tests, but `public: false`.
    let catalog = serde_json::json!({
        "images": [
            { "id": IMAGE_ID, "file": "disk.img", "name": "Disk", "public": false }
        ]
    })
    .to_string();
    tokio::fs::write(dir.path().join("manifest.json"), catalog)
        .await
        .expect("write manifest.json");

    let chunk_root = dir.path().join("chunked").join(IMAGE_ID).join("v1");
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");
    tokio::fs::write(
        chunk_root.join("manifest.json"),
        b"{\"schema\":\"aero.chunked-disk-image.v1\"}",
    )
    .await
    .expect("write chunked manifest");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"ab")
        .await
        .expect("write chunk");

    let store = Arc::new(LocalFsImageStore::new(dir.path()).with_require_manifest(true));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    for uri in [
        "/v1/images/disk/chunked/v1/manifest.json",
        "/v1/images/disk/chunked/v1/chunks/00000000.bin",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
            "private, no-store, no-transform",
            "{uri}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_chunk_name_is_rejected_without_traversal() {
    let (app, dir, _manifest) = setup_app(None).await;

    // A file that would be leaked if the server allowed `..` traversal.
    tokio::fs::write(dir.path().join("secret.bin"), b"top secret")
        .await
        .expect("write secret file");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/..%2Fsecret.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_chunked_version_is_rejected_without_traversal() {
    let (app, dir, _manifest) = setup_app(None).await;

    // A file that would be leaked if the server allowed `..` traversal in the version segment.
    tokio::fs::create_dir_all(dir.path().join("chunked/secret"))
        .await
        .expect("create secret dir");
    tokio::fs::write(dir.path().join("chunked/secret/manifest.json"), b"top secret")
        .await
        .expect("write secret manifest");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/..%2Fsecret/manifest.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overly_long_raw_chunked_version_segment_is_rejected_early() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let too_long_raw = "a".repeat(aero_storage_server::store::MAX_IMAGE_ID_LEN * 3 + 1);
    let uri = format!("/v1/images/disk/chunked/{too_long_raw}/manifest.json");

    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunk_larger_than_limit_is_rejected() {
    let dir = tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("disk.img"), b"raw image bytes")
        .await
        .expect("write disk.img");
    let catalog = serde_json::json!({
        "images": [
            { "id": IMAGE_ID, "file": "disk.img", "name": "Disk", "public": true }
        ]
    })
    .to_string();
    tokio::fs::write(dir.path().join("manifest.json"), catalog)
        .await
        .expect("write manifest.json");

    let chunk_root = dir.path().join("chunked").join(IMAGE_ID);
    tokio::fs::create_dir_all(chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");
    tokio::fs::write(chunk_root.join("manifest.json"), b"{\"schema\":\"aero.chunked-disk-image.v1\"}")
        .await
        .expect("write chunked manifest.json");

    // 5 bytes, but set max to 4.
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"hello")
        .await
        .expect("write chunk");

    let store = Arc::new(LocalFsImageStore::new(dir.path()).with_require_manifest(true));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_max_chunk_bytes(4);
    let app = http::router_with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}
