#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use tower::ServiceExt;

#[tokio::test]
async fn metrics_disabled_returns_404() {
    let store = Arc::new(LocalFsImageStore::new("."));
    let app = aero_storage_server::app(AppState::new(store).with_disable_metrics(true));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(resp.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn metrics_auth_token_is_required() {
    let store = Arc::new(LocalFsImageStore::new("."));
    let token = "test-token";
    let app = aero_storage_server::app(AppState::new(store).with_metrics_auth_token(token));

    let unauth = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(unauth.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
        "expected 401/403, got {}",
        unauth.status()
    );

    let auth = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(auth.status(), StatusCode::OK);
}

