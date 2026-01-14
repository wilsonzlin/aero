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

async fn setup_versioned_app() -> (axum::Router, tempfile::TempDir, String) {
    let dir = tempdir().expect("tempdir");

    // Backing image file required by the image catalog.
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
    let state = ImagesState::new(store, metrics);

    (http::router_with_state(state), dir, manifest)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_endpoints_do_not_require_raw_image_file() {
    let dir = tempdir().expect("tempdir");

    // Image catalog manifest.json references `disk.img`, but we intentionally do NOT create the
    // backing raw file. Chunked endpoints should still work as long as chunked artifacts exist.
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

    let expected_manifest = serde_json::json!({
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
    tokio::fs::write(
        chunk_root.join("manifest.json"),
        expected_manifest.as_bytes(),
    )
    .await
    .expect("write chunked manifest.json");
    tokio::fs::write(chunk_root.join("chunks/00000000.bin"), b"ab")
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
    assert_eq!(std::str::from_utf8(&body).unwrap(), expected_manifest);

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
    assert_eq!(&body[..], b"ab");
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
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["x-content-type-options"].to_str().unwrap(),
        "nosniff"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        resp.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        expected_manifest.as_bytes().len().to_string()
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
async fn chunked_manifest_alias_endpoint_works() {
    let (app, _dir, expected_manifest) = setup_app(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/manifest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), expected_manifest);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_manifest_head_has_expected_headers_and_empty_body() {
    let (app, _dir, expected_manifest) = setup_app(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
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
        resp.headers()["x-content-type-options"].to_str().unwrap(),
        "nosniff"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        resp.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        expected_manifest.as_bytes().len().to_string()
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
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_manifest_with_matching_if_none_match_returns_304() {
    let (app, _dir, _expected_manifest) = setup_app(None).await;

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
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/manifest.json")
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(resp.headers()[header::ETAG].to_str().unwrap(), etag);
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_manifest_with_matching_if_modified_since_returns_304() {
    let (app, _dir, _expected_manifest) = setup_app(None).await;

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
    let last_modified = resp.headers()[header::LAST_MODIFIED]
        .to_str()
        .unwrap()
        .to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/manifest.json")
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert_eq!(
        resp.headers()[header::LAST_MODIFIED].to_str().unwrap(),
        last_modified
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_manifest_head_has_expected_headers_and_empty_body() {
    let (app, _dir, expected_manifest) = setup_versioned_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
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
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["x-content-type-options"].to_str().unwrap(),
        "nosniff"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        resp.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        expected_manifest.as_bytes().len().to_string()
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
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_manifest_alias_endpoint_works() {
    let (app, _dir, expected_manifest) = setup_versioned_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/manifest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), expected_manifest);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_manifest_with_matching_if_none_match_returns_304() {
    let (app, _dir, _expected_manifest) = setup_versioned_app().await;

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
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/manifest.json")
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(resp.headers()[header::ETAG].to_str().unwrap(), etag);
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_manifest_with_matching_if_modified_since_returns_304() {
    let (app, _dir, _expected_manifest) = setup_versioned_app().await;

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
    let last_modified = resp.headers()[header::LAST_MODIFIED]
        .to_str()
        .unwrap()
        .to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/manifest.json")
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert_eq!(
        resp.headers()[header::LAST_MODIFIED].to_str().unwrap(),
        last_modified
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
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
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["x-content-type-options"].to_str().unwrap(),
        "nosniff"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
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
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(resp.headers()["x-content-type-options"].to_str().unwrap(), "nosniff");
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
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
    assert_eq!(resp.headers()["x-content-type-options"].to_str().unwrap(), "nosniff");
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
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
async fn chunked_endpoints_ignore_range_headers() {
    let (app, _dir, expected_manifest) = setup_app(None).await;

    // Even if a client sends `Range`, chunked delivery should not turn into a 206 response.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/manifest.json")
                .header(header::RANGE, "bytes=0-0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), expected_manifest);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .header(header::RANGE, "bytes=0-0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ab");

    // Repeat for versioned endpoints.
    let (app, _dir, expected_manifest) = setup_versioned_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/manifest.json")
                .header(header::RANGE, "bytes=0-0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), expected_manifest);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/chunks/00000000.bin")
                .header(header::RANGE, "bytes=0-0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ab");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_chunk_head_has_expected_headers_and_empty_body() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(resp.headers()["x-content-type-options"].to_str().unwrap(), "nosniff");
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(resp.headers()[header::CONTENT_LENGTH].to_str().unwrap(), "2");
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
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_chunk_with_matching_if_none_match_returns_304() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(resp.headers()[header::ETAG].to_str().unwrap(), etag);
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_chunk_with_matching_if_modified_since_returns_304() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();
    let last_modified = resp.headers()[header::LAST_MODIFIED]
        .to_str()
        .unwrap()
        .to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/chunks/00000000.bin")
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(resp.headers()[header::ETAG].to_str().unwrap(), etag);
    assert_eq!(
        resp.headers()[header::LAST_MODIFIED].to_str().unwrap(),
        last_modified
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_chunk_head_has_expected_headers_and_empty_body() {
    let (app, _dir, _manifest) = setup_versioned_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(resp.headers()["x-content-type-options"].to_str().unwrap(), "nosniff");
    assert!(resp.headers().contains_key(header::ETAG));
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(resp.headers()[header::CONTENT_LENGTH].to_str().unwrap(), "2");
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
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_chunk_with_matching_if_none_match_returns_304() {
    let (app, _dir, _manifest) = setup_versioned_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/chunks/00000000.bin")
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(resp.headers()[header::ETAG].to_str().unwrap(), etag);
    assert!(resp.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_chunked_chunk_with_matching_if_modified_since_returns_304() {
    let (app, _dir, _manifest) = setup_versioned_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/chunks/00000000.bin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();
    let last_modified = resp.headers()[header::LAST_MODIFIED]
        .to_str()
        .unwrap()
        .to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/images/disk/chunked/v1/chunks/00000000.bin")
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable, no-transform"
    );
    assert_eq!(resp.headers()[header::ETAG].to_str().unwrap(), etag);
    assert_eq!(
        resp.headers()[header::LAST_MODIFIED].to_str().unwrap(),
        last_modified
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
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
async fn public_chunked_responses_with_authorization_are_not_publicly_cacheable() {
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
                    .header(header::AUTHORIZATION, "Bearer deadbeef")
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

    // Repeat for explicit versioned endpoints.
    let (app, _dir, _manifest) = setup_versioned_app().await;
    for uri in [
        "/v1/images/disk/chunked/v1/manifest.json",
        "/v1/images/disk/chunked/v1/chunks/00000000.bin",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::AUTHORIZATION, "Bearer deadbeef")
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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overly_long_raw_chunk_name_segment_is_rejected_early() {
    let (app, _dir, _manifest) = setup_app(None).await;

    // `CHUNK_NAME_LEN` in the handler is 12, so reject anything longer than 12*3 raw chars.
    let too_long_raw = "a".repeat(12 * 3 + 1);
    let uri = format!("/v1/images/disk/chunked/chunks/{too_long_raw}");

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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overly_long_raw_chunk_name_segment_is_rejected_early_for_versioned_route() {
    let (app, _dir, _manifest) = setup_app(None).await;

    let too_long_raw = "a".repeat(12 * 3 + 1);
    let uri = format!("/v1/images/disk/chunked/v1/chunks/{too_long_raw}");

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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
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
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
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
    assert!(body.is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_symlink_escape_is_blocked_for_chunk_objects() {
    use std::os::unix::fs::symlink;

    let root_dir = tempdir().expect("tempdir");
    let outside_dir = tempdir().expect("tempdir");

    // Directory listing fallback: image_id must correspond to an on-disk file.
    tokio::fs::write(root_dir.path().join(IMAGE_ID), b"raw image bytes")
        .await
        .expect("write image file");

    let outside_path = outside_dir.path().join("secret.bin");
    tokio::fs::write(&outside_path, b"TOP-SECRET")
        .await
        .expect("write outside file");

    let chunk_dir = root_dir.path().join("chunked").join(IMAGE_ID).join("chunks");
    tokio::fs::create_dir_all(&chunk_dir)
        .await
        .expect("create chunk dirs");

    // Create a symlink that escapes the images root.
    let link_path = chunk_dir.join("00000000.bin");
    symlink(&outside_path, &link_path).expect("create symlink");

    let store = Arc::new(LocalFsImageStore::new(root_dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/images/{IMAGE_ID}/chunked/chunks/00000000.bin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_symlink_escape_is_blocked_for_manifests() {
    use std::os::unix::fs::symlink;

    let root_dir = tempdir().expect("tempdir");
    let outside_dir = tempdir().expect("tempdir");

    tokio::fs::write(root_dir.path().join(IMAGE_ID), b"raw image bytes")
        .await
        .expect("write image file");

    let outside_path = outside_dir.path().join("secret.json");
    tokio::fs::write(&outside_path, b"{\"leak\":true}")
        .await
        .expect("write outside file");

    let image_chunk_root = root_dir.path().join("chunked").join(IMAGE_ID);
    tokio::fs::create_dir_all(image_chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");

    let link_path = image_chunk_root.join("manifest.json");
    symlink(&outside_path, &link_path).expect("create symlink");

    let store = Arc::new(LocalFsImageStore::new(root_dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/images/{IMAGE_ID}/chunked/manifest.json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_symlink_escape_is_blocked_for_versioned_chunk_objects() {
    use std::os::unix::fs::symlink;

    let root_dir = tempdir().expect("tempdir");
    let outside_dir = tempdir().expect("tempdir");

    // Directory listing fallback: image_id must correspond to an on-disk file.
    tokio::fs::write(root_dir.path().join(IMAGE_ID), b"raw image bytes")
        .await
        .expect("write image file");

    let outside_path = outside_dir.path().join("secret.bin");
    tokio::fs::write(&outside_path, b"TOP-SECRET")
        .await
        .expect("write outside file");

    let chunk_dir = root_dir
        .path()
        .join("chunked")
        .join(IMAGE_ID)
        .join("v1")
        .join("chunks");
    tokio::fs::create_dir_all(&chunk_dir)
        .await
        .expect("create chunk dirs");

    let link_path = chunk_dir.join("00000000.bin");
    symlink(&outside_path, &link_path).expect("create symlink");

    let store = Arc::new(LocalFsImageStore::new(root_dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/images/{IMAGE_ID}/chunked/v1/chunks/00000000.bin"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_symlink_escape_is_blocked_for_versioned_manifests() {
    use std::os::unix::fs::symlink;

    let root_dir = tempdir().expect("tempdir");
    let outside_dir = tempdir().expect("tempdir");

    tokio::fs::write(root_dir.path().join(IMAGE_ID), b"raw image bytes")
        .await
        .expect("write image file");

    let outside_path = outside_dir.path().join("secret.json");
    tokio::fs::write(&outside_path, b"{\"leak\":true}")
        .await
        .expect("write outside file");

    let image_chunk_root = root_dir.path().join("chunked").join(IMAGE_ID).join("v1");
    tokio::fs::create_dir_all(image_chunk_root.join("chunks"))
        .await
        .expect("create chunk dirs");

    let link_path = image_chunk_root.join("manifest.json");
    symlink(&outside_path, &link_path).expect("create symlink");

    let store = Arc::new(LocalFsImageStore::new(root_dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics);
    let app = http::router_with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/images/{IMAGE_ID}/chunked/v1/manifest.json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        resp.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert_eq!(
        resp.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap(),
        "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length"
    );
    assert_eq!(
        resp.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}
