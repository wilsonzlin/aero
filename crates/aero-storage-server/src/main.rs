use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{routing::get, Router};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let listen_addr: SocketAddr = env::var("AERO_STORAGE_LISTEN_ADDR")
        .or_else(|_| env::var("AERO_BIND"))
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()?;

    let image_root = PathBuf::from(
        env::var("AERO_STORAGE_IMAGE_ROOT")
            .or_else(|_| env::var("AERO_IMAGE_ROOT"))
            .unwrap_or_else(|_| "./images".to_string()),
    );

    let store = Arc::new(LocalFsImageStore::new(&image_root));
    let app = Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        .merge(aero_storage_server::app(AppState::new(store)));

    tracing::info!(
        "aero-storage-server listening on http://{listen_addr} (root: {})",
        image_root.display()
    );

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
