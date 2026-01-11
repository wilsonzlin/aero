pub mod api;
pub mod http;
pub mod metrics;
pub mod server;
pub mod store;

use std::sync::Arc;

use axum::http::HeaderValue;
use axum::middleware;

use metrics::Metrics;
use store::ImageStore;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn ImageStore>,
    pub cors_allow_origin: HeaderValue,
    pub cors_allow_credentials: bool,
}

impl AppState {
    pub fn new(store: Arc<dyn ImageStore>) -> Self {
        Self {
            store,
            cors_allow_origin: HeaderValue::from_static("*"),
            cors_allow_credentials: false,
        }
    }

    pub fn with_cors_allow_origin(mut self, cors_allow_origin: HeaderValue) -> Self {
        self.cors_allow_origin = cors_allow_origin;
        self
    }

    pub fn with_cors_allow_credentials(mut self, cors_allow_credentials: bool) -> Self {
        self.cors_allow_credentials = cors_allow_credentials;
        self
    }
}

pub fn app(state: AppState) -> axum::Router {
    let cors_allow_origin = state.cors_allow_origin.clone();
    let cors_allow_credentials = state.cors_allow_credentials;
    let store = Arc::clone(&state.store);
    let metrics = Arc::new(Metrics::new());

    axum::Router::new()
        .merge(http::router_with_state(
            http::images::ImagesState::new(store, Arc::clone(&metrics))
                .with_cors_allow_origin(cors_allow_origin)
                .with_cors_allow_credentials(cors_allow_credentials),
        ))
        .merge(api::router(state))
        .route_layer(middleware::from_fn_with_state(
            metrics,
            http::observability::middleware,
        ))
}
pub use server::{start, RunningStorageServer, StorageServerConfig};
