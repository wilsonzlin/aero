#[cfg(not(target_arch = "wasm32"))]
pub mod api;
#[cfg(not(target_arch = "wasm32"))]
pub mod cors;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod headers;
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
/// Default maximum number of concurrent requests allowed for the image bytes endpoints
/// (`/v1/images/:image_id` and `/v1/images/:image_id/data`).
///
/// This is a per-process cap intended as basic DoS hardening.
pub const DEFAULT_MAX_CONCURRENT_BYTES_REQUESTS: usize = 64;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn ImageStore>,
    pub cors: CorsConfig,
    pub cross_origin_resource_policy: HeaderValue,
    range_options: Option<http::range::RangeOptions>,
    public_cache_max_age: Option<Duration>,
    max_concurrent_bytes_requests: usize,
    require_range: bool,
    metrics_endpoint_disabled: bool,
    metrics_auth_token: Option<Arc<str>>,
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
            max_concurrent_bytes_requests: DEFAULT_MAX_CONCURRENT_BYTES_REQUESTS,
            require_range: false,
            metrics_endpoint_disabled: false,
            metrics_auth_token: None,
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

    /// Set the maximum number of concurrent requests allowed to the image bytes endpoints.
    ///
    /// Use `0` to disable limiting (unlimited).
    pub fn with_max_concurrent_bytes_requests(mut self, max: usize) -> Self {
        self.max_concurrent_bytes_requests = max;
        self
    }

    pub fn with_require_range(mut self, require_range: bool) -> Self {
        self.require_range = require_range;
        self
    }

    /// Disable the `/metrics` endpoint entirely (it will not be mounted, so requests return `404`).
    pub fn with_disable_metrics(mut self, disable_metrics: bool) -> Self {
        self.metrics_endpoint_disabled = disable_metrics;
        self
    }

    /// Require `Authorization: Bearer <token>` for the `/metrics` endpoint.
    pub fn with_metrics_auth_token(mut self, metrics_auth_token: impl Into<String>) -> Self {
        self.metrics_auth_token = Some(Arc::from(metrics_auth_token.into().into_boxed_str()));
        self
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn app(state: AppState) -> axum::Router {
    let cors = state.cors.clone();
    let cross_origin_resource_policy = state.cross_origin_resource_policy.clone();
    let range_options = state.range_options;
    let public_cache_max_age = state.public_cache_max_age;
    let max_concurrent_bytes_requests = state.max_concurrent_bytes_requests;
    let require_range = state.require_range;
    let metrics_endpoint_disabled = state.metrics_endpoint_disabled;
    let metrics_auth_token = state.metrics_auth_token.clone();
    let store = Arc::clone(&state.store);
    let metrics = Arc::new(Metrics::new());

    let mut images_state = http::images::ImagesState::new(store, Arc::clone(&metrics))
        .with_cors(cors)
        .with_cross_origin_resource_policy(cross_origin_resource_policy)
        .with_max_concurrent_bytes_requests(max_concurrent_bytes_requests)
        .with_require_range(require_range)
        .with_metrics_endpoint_disabled(metrics_endpoint_disabled)
        .with_metrics_auth_token(metrics_auth_token);
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
