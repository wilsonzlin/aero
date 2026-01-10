use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::body::Body;
use axum::extract::{FromRef, State};
use axum::http::header;
use axum::http::{Response, StatusCode};
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::http::images::{self, ImagesState};
use crate::store::{ImageStore, LocalFsImageStore};

#[derive(Clone, Debug)]
pub struct StorageServerConfig {
    pub bind_addr: SocketAddr,
    pub images_dir: PathBuf,
}

#[derive(Clone)]
struct AppState {
    images_dir: PathBuf,
    images: ImagesState,
}

impl FromRef<AppState> for ImagesState {
    fn from_ref(state: &AppState) -> Self {
        state.images.clone()
    }
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
    let images = ImagesState::new(store);

    let app_state = AppState {
        images_dir: config.images_dir.clone(),
        images,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/healthz", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .route(
            "/v1/images/:image_id",
            get(images::get_image)
                .head(images::head_image)
                .options(images::options_image),
        )
        .with_state(app_state);

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

async fn ready(State(state): State<AppState>) -> StatusCode {
    match std::fs::metadata(&state.images_dir) {
        Ok(metadata) if metadata.is_dir() => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn metrics() -> Response<Body> {
    let body = format!(
        r#"# HELP aero_storage_server_build_info Build information for aero-storage-server.
# TYPE aero_storage_server_build_info gauge
aero_storage_server_build_info{{version="{version}"}} 1
"#,
        version = env!("CARGO_PKG_VERSION")
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
        .body(Body::from(body))
        .expect("valid response")
}
