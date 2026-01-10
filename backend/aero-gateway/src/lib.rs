mod admin;
mod config;
mod dns;
mod stats;
mod tcp_proxy;

pub mod capture;

use axum::{routing::get, Router};

pub use config::GatewayConfig;
pub use dns::DnsCache;
pub use stats::{Stats, StatsSnapshot};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) admin_api_key: Option<String>,
    pub(crate) stats: Stats,
    pub(crate) dns_cache: DnsCache,
    pub(crate) capture: capture::CaptureManager,
}

impl AppState {
    pub(crate) async fn new(config: GatewayConfig) -> std::io::Result<Self> {
        Ok(Self {
            admin_api_key: config.admin_api_key,
            stats: Stats::new(),
            dns_cache: DnsCache::new(),
            capture: capture::CaptureManager::new(config.capture).await?,
        })
    }
}

/// Build the Aero Gateway HTTP router.
///
/// Admin endpoints are always registered but will return `404` unless an
/// `admin_api_key` is configured.
pub async fn build_app(config: GatewayConfig) -> std::io::Result<Router> {
    let state = AppState::new(config).await?;

    Ok(Router::new()
        .route("/tcp", get(tcp_proxy::tcp_ws_handler))
        .nest("/admin", admin::router())
        .with_state(state))
}
