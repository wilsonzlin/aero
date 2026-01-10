use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::store::{ImageStore, LocalFsImageStore};
use crate::AppState;

#[derive(Clone, Debug)]
pub struct StorageServerConfig {
    pub bind_addr: SocketAddr,
    pub images_dir: PathBuf,
}

pub struct RunningStorageServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<anyhow::Result<()>>>,
}

impl RunningStorageServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        if let Some(join) = self.join.take() {
            join.await.context("storage server task panicked")??;
        }
        Ok(())
    }
}

impl Drop for RunningStorageServer {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

pub async fn start(config: StorageServerConfig) -> anyhow::Result<RunningStorageServer> {
    tokio::fs::create_dir_all(&config.images_dir)
        .await
        .with_context(|| format!("create images dir {}", config.images_dir.display()))?;

    let store: Arc<dyn ImageStore> = Arc::new(LocalFsImageStore::new(&config.images_dir));
    let images_dir = Arc::new(config.images_dir.clone());
    let app = crate::app(AppState::new(store));

    let app = Router::new()
        .route("/health", get(health))
        .route("/healthz", get(health))
        .route("/ready", get({
            let images_dir = Arc::clone(&images_dir);
            move || {
                let images_dir = Arc::clone(&images_dir);
                async move {
                    match std::fs::metadata(&*images_dir) {
                        Ok(metadata) if metadata.is_dir() => StatusCode::OK,
                        _ => StatusCode::SERVICE_UNAVAILABLE,
                    }
                }
            }
        }))
        .merge(app);

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("bind {}", config.bind_addr))?;
    let addr = listener.local_addr().context("read bound address")?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .context("serve")?;
        Ok(())
    });

    Ok(RunningStorageServer {
        addr,
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    })
}

async fn health() -> &'static str {
    "ok\n"
}
