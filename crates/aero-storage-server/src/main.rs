mod config;

use crate::config::Config;
use aero_storage_server::{store::LocalFsImageStore, AppState};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load();
    init_tracing(&config.log_level)?;

    tokio::fs::create_dir_all(&config.images_root).await?;

    let store = Arc::new(LocalFsImageStore::new(config.images_root.clone()));
    let app = aero_storage_server::app(AppState::new(store));

    tracing::info!(
        listen_addr = %config.listen_addr,
        images_root = %config.images_root.display(),
        cors_origin = ?config.cors_origin,
        "aero-storage-server listening"
    );

    let listener = TcpListener::bind(config.listen_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn init_tracing(log_level: &str) -> Result<(), tracing_subscriber::filter::ParseError> {
    let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(log_level))?;

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(true)
        .init();

    Ok(())
}

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
