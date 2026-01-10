use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};

use crate::{AppState, StatsSnapshot};

pub fn router() -> Router<AppState> {
    Router::new().route("/stats", get(stats))
}

async fn stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatsSnapshot>, StatusCode> {
    authorize(&state, &headers)?;
    Ok(Json(state.stats.snapshot(state.dns_cache.len())))
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(expected) = state.admin_api_key.as_deref() else {
        // Admin endpoints should not be reachable unless explicitly configured.
        return Err(StatusCode::NOT_FOUND);
    };

    let Some(provided) = headers.get("ADMIN_API_KEY") else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
