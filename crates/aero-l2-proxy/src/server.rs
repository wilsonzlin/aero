use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        OriginalUri,
        State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::{sync::oneshot, task::JoinHandle};
use tokio::sync::Semaphore;

use crate::{
    capture::CaptureManager, dns::DnsService, metrics::Metrics, session, ProxyConfig,
    TUNNEL_SUBPROTOCOL,
};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) cfg: Arc<ProxyConfig>,
    pub(crate) dns: Arc<DnsService>,
    pub(crate) l2_limits: aero_l2_protocol::Limits,
    pub(crate) metrics: Metrics,
    pub(crate) capture: CaptureManager,
    pub(crate) connections: Option<Arc<Semaphore>>,
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

    let metrics = Metrics::new();
    let capture = CaptureManager::new(cfg.capture_dir.clone()).await?;

    let connections = (cfg.security.max_connections != 0)
        .then(|| Arc::new(Semaphore::new(cfg.security.max_connections)));

    let state = AppState {
        cfg: Arc::new(cfg),
        dns: Arc::new(dns),
        l2_limits,
        metrics,
        capture,
        connections,
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
        .route("/metrics", get(metrics))
        .route("/l2", get(l2_ws_handler))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    StatusCode::OK
}

async fn readyz() -> impl IntoResponse {
    StatusCode::OK
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let body = state.metrics.render_prometheus();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
}

async fn l2_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> impl IntoResponse {
    if !has_subprotocol(&headers, TUNNEL_SUBPROTOCOL) {
        return (
            StatusCode::BAD_REQUEST,
            format!("missing required websocket subprotocol {TUNNEL_SUBPROTOCOL:?}"),
        )
            .into_response();
    }

    if let Err(resp) = enforce_security(&state, &headers, &uri) {
        return resp;
    }

    let permit = match &state.connections {
        None => None,
        Some(semaphore) => match semaphore.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "max connections exceeded".to_string(),
                )
                    .into_response();
            }
        },
    };

    ws.protocols([TUNNEL_SUBPROTOCOL]).on_upgrade(move |socket| async move {
        let _permit = permit;
        handle_l2_ws(socket, state).await;
    })
}

async fn handle_l2_ws(socket: WebSocket, state: AppState) {
    let session_id = state.metrics.next_session_id();
    if let Err(err) = session::run_session(socket, state, session_id).await {
        tracing::debug!(session_id, "l2 session ended: {err:#}");
    }
}

fn enforce_security(
    state: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
) -> Result<(), axum::response::Response> {
    if !state.cfg.security.open {
        let origin = headers
            .get(axum::http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty());

        let Some(origin) = origin else {
            return Err((StatusCode::FORBIDDEN, "missing Origin header".to_string()).into_response());
        };

        match &state.cfg.security.allowed_origins {
            crate::config::AllowedOrigins::Any => {}
            crate::config::AllowedOrigins::List(list) => {
                if !list.iter().any(|allowed| allowed == origin) {
                    return Err((
                        StatusCode::FORBIDDEN,
                        format!("Origin not allowed: {origin}"),
                    )
                        .into_response());
                }
            }
        }
    }

    if let Some(expected) = state.cfg.security.token.as_deref() {
        let query_token = token_from_query(uri);
        let protocol_token = token_from_subprotocol(headers);
        let token_ok = query_token.as_deref() == Some(expected)
            || protocol_token.as_deref() == Some(expected);
        if !token_ok {
            return Err((StatusCode::UNAUTHORIZED, "invalid token".to_string()).into_response());
        }
    }

    Ok(())
}

fn token_from_query(uri: &axum::http::Uri) -> Option<String> {
    let query = uri.query()?;
    for part in query.split('&') {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        if k == "token" {
            return (!v.is_empty()).then(|| percent_decode(v));
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = from_hex(bytes[i + 1]);
            let lo = from_hex(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn token_from_subprotocol(headers: &HeaderMap) -> Option<String> {
    let Some(value) = headers
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
    else {
        return None;
    };

    value.split(',').map(str::trim).find_map(|proto| {
        proto
            .strip_prefix("aero-l2-token.")
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
    })
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
