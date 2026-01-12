#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extremely_long_image_id_does_not_bloat_error_body() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let app = aero_storage_server::app(AppState::new(store));

    // Use a much larger value than the 128-char cap to ensure we don't accidentally echo the
    // entire ID back in error responses.
    let long_id = "a".repeat(10_000);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/images/{long_id}/meta"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(
        body.len() <= 1024,
        "expected a bounded error body, got {} bytes",
        body.len()
    );
}

