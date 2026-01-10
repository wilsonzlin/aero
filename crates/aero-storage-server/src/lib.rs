pub mod api;
pub mod http;
pub mod server;
pub mod store;

use std::sync::Arc;

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
    axum::Router::new()
        .merge(http::images::router(store))
        .merge(api::router(state))
}
pub use server::{start, RunningStorageServer, StorageServerConfig};
