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
use tokio::net::TcpListener;
#[cfg(not(target_arch = "wasm32"))]
use tracing_subscriber::EnvFilter;

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load();
    init_tracing(&config.log_level)?;

    tokio::fs::create_dir_all(&config.images_root).await?;

    let store = Arc::new(LocalFsImageStore::new(config.images_root.clone()));
    let mut state = AppState::new(store);
    if let Some(origin) = config.cors_origin.as_deref() {
        let origin = origin.trim();
        state = state.with_cors_allow_origin(HeaderValue::from_str(origin)?);
        // If an explicit origin is configured (i.e. not `*`), default to allowing credentials so
        // cookie-authenticated requests can succeed in cross-origin deployments.
        state = state.with_cors_allow_credentials(origin != "*");
    }
    let corp_policy = config.cross_origin_resource_policy.trim();
    if corp_policy != "same-origin" && corp_policy != "same-site" && corp_policy != "cross-origin" {
        anyhow::bail!(
            "invalid cross-origin resource policy: {corp_policy} (expected same-origin, same-site, or cross-origin)"
        );
    }
    state = state.with_cross_origin_resource_policy(HeaderValue::from_str(corp_policy)?);
    let app = aero_storage_server::app(state);

    tracing::info!(
        listen_addr = %config.listen_addr,
        images_root = %config.images_root.display(),
        cors_origin = ?config.cors_origin,
        cross_origin_resource_policy = %corp_policy,
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
