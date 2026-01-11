mod images;

use axum::{routing::get, Json, Router};
use serde::Serialize;

use crate::AppState;

#[derive(Debug, Serialize)]
struct StatusResponse {
    status: &'static str,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/healthz", get(healthz))
        .route("/ready", get(readyz))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .merge(images::router())
        .with_state(state)
}

async fn health() -> &'static str {
    "ok\n"
}

async fn healthz() -> Json<StatusResponse> {
    Json(StatusResponse { status: "ok" })
}

async fn readyz() -> Json<StatusResponse> {
    Json(StatusResponse { status: "ok" })
}

#[derive(Debug, Serialize)]
struct VersionResponse {
    version: String,
    #[serde(rename = "gitSha")]
    git_sha: String,
    #[serde(rename = "builtAt")]
    built_at: String,
}

async fn version() -> Json<VersionResponse> {
    let version = std::env::var("AERO_STORAGE_SERVER_VERSION")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let git_sha = std::env::var("AERO_STORAGE_SERVER_GIT_SHA")
        .or_else(|_| std::env::var("GIT_SHA"))
        .unwrap_or_else(|_| "dev".to_string());
    let built_at = std::env::var("AERO_STORAGE_SERVER_BUILD_TIMESTAMP")
        .or_else(|_| std::env::var("BUILD_TIMESTAMP"))
        .unwrap_or_default();

    Json(VersionResponse {
        version,
        git_sha,
        built_at,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::{store::LocalFsImageStore, AppState};

    #[tokio::test]
    async fn version_endpoint_smoke_test() {
        let store = Arc::new(LocalFsImageStore::new("."));
        let app = crate::app(AppState::new(store));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed.get("version").and_then(|v| v.as_str()).is_some());
        assert!(parsed.get("gitSha").and_then(|v| v.as_str()).is_some());
        assert!(parsed.get("builtAt").and_then(|v| v.as_str()).is_some());
    }
}
