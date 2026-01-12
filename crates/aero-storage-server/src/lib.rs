#[cfg(not(target_arch = "wasm32"))]
pub mod api;
#[cfg(not(target_arch = "wasm32"))]
pub mod cors;
#[cfg(not(target_arch = "wasm32"))]
pub mod http;
#[cfg(not(target_arch = "wasm32"))]
pub mod metrics;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use axum::http::HeaderValue;
#[cfg(not(target_arch = "wasm32"))]
use axum::middleware;

#[cfg(not(target_arch = "wasm32"))]
use cors::CorsConfig;
#[cfg(not(target_arch = "wasm32"))]
use metrics::Metrics;
#[cfg(not(target_arch = "wasm32"))]
use store::ImageStore;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn ImageStore>,
    pub cors: CorsConfig,
    pub cross_origin_resource_policy: HeaderValue,
    range_options: Option<http::range::RangeOptions>,
    public_cache_max_age: Option<Duration>,
}

#[cfg(not(target_arch = "wasm32"))]
impl AppState {
    pub fn new(store: Arc<dyn ImageStore>) -> Self {
        Self {
            store,
            cors: CorsConfig::default(),
            cross_origin_resource_policy: HeaderValue::from_static("same-site"),
            range_options: None,
            public_cache_max_age: None,
        }
    }

    pub fn with_cors_allow_origin(mut self, cors_allow_origin: HeaderValue) -> Self {
        self.cors = self.cors.with_allow_origin(cors_allow_origin);
        self
    }

    pub fn with_cors_allow_credentials(mut self, cors_allow_credentials: bool) -> Self {
        self.cors = self.cors.with_allow_credentials(cors_allow_credentials);
        self
    }

    pub fn with_cors_allowed_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.cors = self.cors.with_allowed_origins(origins);
        self
    }

    pub fn with_cors_preflight_max_age(mut self, max_age: Duration) -> Self {
        self.cors = self.cors.with_preflight_max_age(max_age);
        self
    }

    pub fn with_cross_origin_resource_policy(
        mut self,
        cross_origin_resource_policy: HeaderValue,
    ) -> Self {
        self.cross_origin_resource_policy = cross_origin_resource_policy;
        self
    }

    /// Configure the maximum number of bytes allowed to be served in response to a single `Range`
    /// request.
    pub fn with_max_range_bytes(mut self, max_total_bytes: u64) -> Self {
        self.range_options = Some(http::range::RangeOptions { max_total_bytes });
        self
    }

    pub fn with_public_cache_max_age(mut self, max_age: Duration) -> Self {
        self.public_cache_max_age = Some(max_age);
        self
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn app(state: AppState) -> axum::Router {
    let cors = state.cors.clone();
    let cross_origin_resource_policy = state.cross_origin_resource_policy.clone();
    let range_options = state.range_options;
    let public_cache_max_age = state.public_cache_max_age;
    let store = Arc::clone(&state.store);
    let metrics = Arc::new(Metrics::new());

    let mut images_state = http::images::ImagesState::new(store, Arc::clone(&metrics))
        .with_cors(cors)
        .with_cross_origin_resource_policy(cross_origin_resource_policy);
    if let Some(range_options) = range_options {
        images_state = images_state.with_range_options(range_options);
    }
    if let Some(max_age) = public_cache_max_age {
        images_state = images_state.with_public_cache_max_age(max_age);
    }

    axum::Router::new()
        .merge(http::router_with_state(images_state))
        .merge(api::router(state))
        .route_layer(middleware::from_fn_with_state(
            metrics,
            http::observability::middleware,
        ))
}

#[cfg(not(target_arch = "wasm32"))]
pub use server::{start, RunningStorageServer, StorageServerConfig};
