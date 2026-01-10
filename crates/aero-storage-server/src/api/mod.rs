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

