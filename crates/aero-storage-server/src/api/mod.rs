mod images;

use axum::Router;

use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new().merge(images::router()).with_state(state)
}
