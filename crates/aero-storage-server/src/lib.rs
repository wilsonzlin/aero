pub mod api;
pub mod http;
pub mod metrics;
pub mod server;
pub mod store;

use std::sync::Arc;

use axum::middleware;

use metrics::Metrics;
use store::ImageStore;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn ImageStore>,
}

impl AppState {
    pub fn new(store: Arc<dyn ImageStore>) -> Self {
        Self { store }
    }
}

pub fn app(state: AppState) -> axum::Router {
    let store = Arc::clone(&state.store);
    let metrics = Arc::new(Metrics::new());

    axum::Router::new()
        .merge(http::router(store, Arc::clone(&metrics)))
        .merge(api::router(state))
        .route_layer(middleware::from_fn_with_state(
            metrics,
            http::observability::middleware,
        ))
}
pub use server::{start, RunningStorageServer, StorageServerConfig};
