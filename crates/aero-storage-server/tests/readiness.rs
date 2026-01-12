#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readyz_returns_200_when_store_is_healthy() {
    let dir = tempfile::tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("test.img"), vec![0_u8; 16])
        .await
        .expect("write image");
    tokio::fs::write(
        dir.path().join("manifest.json"),
        r#"{
          "images": [
            { "id": "test", "file": "test.img", "name": "Test", "public": true }
          ]
        }"#,
    )
    .await
    .expect("write manifest");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store).with_cors_allow_origin("https://example.com".parse().unwrap());
    let app = aero_storage_server::app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .header(header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "https://example.com"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readyz_returns_non_200_when_manifest_is_invalid() {
    let dir = tempfile::tempdir().expect("tempdir");

    tokio::fs::write(dir.path().join("manifest.json"), b"{ this is not json")
        .await
        .expect("write manifest");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store).with_cors_allow_origin("https://example.com".parse().unwrap());
    let app = aero_storage_server::app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .header(header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "https://example.com"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "error");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readyz_returns_non_200_when_manifest_has_invalid_etag() {
    let dir = tempfile::tempdir().expect("tempdir");

    tokio::fs::write(
        dir.path().join("manifest.json"),
        r#"{
          "images": [
            { "id": "test", "file": "test.img", "name": "Test", "etag": "bad\netag", "public": true }
          ]
        }"#,
    )
    .await
    .expect("write manifest");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store).with_cors_allow_origin("https://example.com".parse().unwrap());
    let app = aero_storage_server::app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .header(header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "https://example.com"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "error");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readyz_returns_non_200_when_manifest_has_invalid_last_modified() {
    let dir = tempfile::tempdir().expect("tempdir");

    tokio::fs::write(
        dir.path().join("manifest.json"),
        r#"{
          "images": [
            { "id": "test", "file": "test.img", "name": "Test", "last_modified": "not-a-date", "public": true }
          ]
        }"#,
    )
    .await
    .expect("write manifest");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store).with_cors_allow_origin("https://example.com".parse().unwrap());
    let app = aero_storage_server::app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .header(header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "https://example.com"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "error");
}
