use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::{sync::oneshot, task::JoinHandle};

use crate::{dns::DnsService, session, ProxyConfig, TUNNEL_SUBPROTOCOL};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) cfg: Arc<ProxyConfig>,
    pub(crate) dns: Arc<DnsService>,
    pub(crate) l2_limits: aero_l2_protocol::Limits,
}

pub struct ServerHandle {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn start_server(cfg: ProxyConfig) -> std::io::Result<ServerHandle> {
    let listener = tokio::net::TcpListener::bind(cfg.bind_addr).await?;
    let addr = listener.local_addr()?;

    let dns = DnsService::new(
        cfg.test_overrides.dns_a.clone(),
        cfg.dns_default_ttl_secs,
        cfg.dns_max_ttl_secs,
    )
    .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;

    let l2_limits = aero_l2_protocol::Limits {
        max_frame_payload: cfg.l2_max_frame_payload,
        max_control_payload: cfg.l2_max_control_payload,
    };

    let state = AppState {
        cfg: Arc::new(cfg),
        dns: Arc::new(dns),
        l2_limits,
    };
    let app = build_app(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(ServerHandle {
        addr,
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
    })
}

fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/l2", get(l2_ws_handler))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    StatusCode::OK
}

async fn readyz() -> impl IntoResponse {
    StatusCode::OK
}

async fn l2_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !has_subprotocol(&headers, TUNNEL_SUBPROTOCOL) {
        return (
            StatusCode::BAD_REQUEST,
            format!("missing required websocket subprotocol {TUNNEL_SUBPROTOCOL:?}"),
        )
            .into_response();
    }

    ws.protocols([TUNNEL_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_l2_ws(socket, state))
}

async fn handle_l2_ws(socket: WebSocket, state: AppState) {
    if let Err(err) = session::run_session(socket, state).await {
        tracing::debug!("l2 session ended: {err:#}");
    }
}

fn has_subprotocol(headers: &HeaderMap, required: &str) -> bool {
    let Some(value) = headers
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };

    value
        .split(',')
        .map(str::trim)
        .any(|proto| proto == required)
}
