mod images;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::AppState;

#[derive(Debug, Serialize)]
struct StatusResponse {
    status: &'static str,
}

pub fn router(state: AppState) -> Router {
    let state_for_guard = state.clone();
    Router::new()
        .route("/health", get(health))
        .route("/healthz", get(healthz))
        .route("/ready", get(ready))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .merge(images::router())
        .with_state(state)
        // DoS hardening: reject pathological `:id` segments before `Path<String>` extraction.
        .route_layer(axum::middleware::from_fn_with_state(
            state_for_guard,
            images::image_id_path_len_guard,
        ))
}

fn insert_cors_headers(headers: &mut HeaderMap, state: &AppState, req_headers: &HeaderMap) {
    state.cors.insert_cors_headers(headers, req_headers, None);
}

async fn health(State(state): State<AppState>, req_headers: HeaderMap) -> Response {
    let mut resp = "ok\n".into_response();
    insert_cors_headers(resp.headers_mut(), &state, &req_headers);
    resp
}

async fn healthz(State(state): State<AppState>, req_headers: HeaderMap) -> Response {
    let mut resp = Json(StatusResponse { status: "ok" }).into_response();
    insert_cors_headers(resp.headers_mut(), &state, &req_headers);
    resp
}

async fn ready(State(state): State<AppState>, req_headers: HeaderMap) -> Response {
    ready_response(state, req_headers).await
}

async fn readyz(State(state): State<AppState>, req_headers: HeaderMap) -> Response {
    ready_response(state, req_headers).await
}

async fn ready_response(state: AppState, req_headers: HeaderMap) -> Response {
    let ready = state.store.list_images().await.is_ok();

    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let mut resp = (
        status,
        Json(StatusResponse {
            status: if ready { "ok" } else { "error" },
        }),
    )
        .into_response();
    insert_cors_headers(resp.headers_mut(), &state, &req_headers);
    resp
}

#[derive(Debug, Serialize)]
struct VersionResponse {
    version: String,
    #[serde(rename = "gitSha")]
    git_sha: String,
    #[serde(rename = "builtAt")]
    built_at: String,
}

async fn version(State(state): State<AppState>, req_headers: HeaderMap) -> Response {
    let version = std::env::var("AERO_STORAGE_SERVER_VERSION")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let git_sha = std::env::var("AERO_STORAGE_SERVER_GIT_SHA")
        .or_else(|_| std::env::var("GIT_SHA"))
        .unwrap_or_else(|_| "dev".to_string());
    let built_at = std::env::var("AERO_STORAGE_SERVER_BUILD_TIMESTAMP")
        .or_else(|_| std::env::var("BUILD_TIMESTAMP"))
        .unwrap_or_default();

    let mut resp = Json(VersionResponse {
        version,
        git_sha,
        built_at,
    })
    .into_response();
    insert_cors_headers(resp.headers_mut(), &state, &req_headers);
    resp
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
