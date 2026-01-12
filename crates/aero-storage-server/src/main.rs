#[cfg(not(target_arch = "wasm32"))]
mod config;

#[cfg(not(target_arch = "wasm32"))]
use crate::config::Config;
#[cfg(not(target_arch = "wasm32"))]
use aero_storage_server::{store::LocalFsImageStore, AppState};
#[cfg(not(target_arch = "wasm32"))]
use axum::http::HeaderValue;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use tokio::net::TcpListener;
#[cfg(not(target_arch = "wasm32"))]
use tracing_subscriber::EnvFilter;

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load();
    init_tracing(&config.log_level)?;

    tokio::fs::create_dir_all(&config.images_root).await?;

    let store = Arc::new(
        LocalFsImageStore::new(config.images_root.clone())
            .with_require_manifest(config.require_manifest),
    );
    let mut state = AppState::new(store);

    if let Some(origins) = config.cors_origins.as_deref() {
        if origins.iter().any(|o| o.trim() == "*") {
            state = state.with_cors_allow_origin(HeaderValue::from_static("*"));
            state = state.with_cors_allow_credentials(false);
        } else {
            state = state.with_cors_allowed_origins(origins.iter().map(|s| s.as_str()));
            // If an explicit allowlist is configured (i.e. not `*`), default to allowing
            // credentials so cookie-authenticated requests can succeed in cross-origin deployments.
            state = state.with_cors_allow_credentials(true);
        }
    }

    if let Some(max_range_bytes) = config.max_range_bytes {
        state = state.with_max_range_bytes(max_range_bytes);
    }

    if let Some(max_age_secs) = config.public_cache_max_age_secs {
        state = state.with_public_cache_max_age(Duration::from_secs(max_age_secs));
    }

    if let Some(max_age_secs) = config.cors_preflight_max_age_secs {
        state = state.with_cors_preflight_max_age(Duration::from_secs(max_age_secs));
    }
    let corp_policy = config.cross_origin_resource_policy.trim();
    if corp_policy != "same-origin" && corp_policy != "same-site" && corp_policy != "cross-origin" {
        anyhow::bail!(
            "invalid cross-origin resource policy: {corp_policy} (expected same-origin, same-site, or cross-origin)"
        );
    }
    state = state.with_cross_origin_resource_policy(HeaderValue::from_str(corp_policy)?);
    state = state.with_max_concurrent_bytes_requests(config.max_concurrent_bytes_requests);
    let app = aero_storage_server::app(state);

    tracing::info!(
        listen_addr = %config.listen_addr,
        images_root = %config.images_root.display(),
        require_manifest = %config.require_manifest,
        cors_origins = ?config.cors_origins,
        cross_origin_resource_policy = %corp_policy,
        max_range_bytes = ?config.max_range_bytes,
        public_cache_max_age_secs = ?config.public_cache_max_age_secs,
        cors_preflight_max_age_secs = ?config.cors_preflight_max_age_secs,
        max_concurrent_bytes_requests = config.max_concurrent_bytes_requests,
        "aero-storage-server listening"
    );

    let listener = TcpListener::bind(config.listen_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn init_tracing(log_level: &str) -> Result<(), tracing_subscriber::filter::ParseError> {
    let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(log_level))?;

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(true)
        .init();

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
