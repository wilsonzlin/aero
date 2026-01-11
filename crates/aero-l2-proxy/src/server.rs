use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        ConnectInfo, OriginalUri, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::{
    sync::{oneshot, watch, Semaphore},
    task::JoinHandle,
};

use base64::Engine;
use ring::hmac;
use serde::Deserialize;

use crate::{
    capture::CaptureManager, dns::DnsService, metrics::Metrics, session, ProxyConfig,
    TUNNEL_SUBPROTOCOL,
};

const SESSION_COOKIE_NAME: &str = "aero_session";

#[derive(Debug, Deserialize)]
struct SessionTokenPayload {
    v: u8,
    sid: String,
    exp: u64,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) cfg: Arc<ProxyConfig>,
    pub(crate) dns: Arc<DnsService>,
    pub(crate) l2_limits: aero_l2_protocol::Limits,
    pub(crate) metrics: Metrics,
    pub(crate) capture: CaptureManager,
    pub(crate) connections: Option<Arc<Semaphore>>,
    pub(crate) shutting_down: Arc<AtomicBool>,
    pub(crate) shutdown_rx: watch::Receiver<bool>,
}

pub struct ServerHandle {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    shutting_down: Arc<AtomicBool>,
    shutdown_broadcast: watch::Sender<bool>,
    shutdown_grace: Duration,
}

impl ServerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn mark_shutting_down(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let _ = self.shutdown_broadcast.send(true);
    }

    pub async fn shutdown(mut self) {
        self.mark_shutting_down();
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let mut task = task;
            if tokio::time::timeout(self.shutdown_grace, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.mark_shutting_down();
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn start_server(cfg: ProxyConfig) -> std::io::Result<ServerHandle> {
    let shutdown_grace = cfg.shutdown_grace;
    let listener = tokio::net::TcpListener::bind(cfg.bind_addr).await?;
    let addr = listener.local_addr()?;

    let dns = DnsService::new(
        cfg.test_overrides.dns_a.clone(),
        cfg.dns_default_ttl_secs,
        cfg.dns_max_ttl_secs,
    )
    .map_err(std::io::Error::other)?;

    let l2_limits = aero_l2_protocol::Limits {
        max_frame_payload: cfg.l2_max_frame_payload,
        max_control_payload: cfg.l2_max_control_payload,
    };

    let metrics = Metrics::new();
    let capture = CaptureManager::new(cfg.capture_dir.clone()).await?;

    let connections = (cfg.security.max_connections != 0)
        .then(|| Arc::new(Semaphore::new(cfg.security.max_connections)));

    let shutting_down = Arc::new(AtomicBool::new(false));
    let (shutdown_broadcast, sessions_shutdown_rx) = watch::channel(false);

    let state = AppState {
        cfg: Arc::new(cfg),
        dns: Arc::new(dns),
        l2_limits,
        metrics,
        capture,
        connections,
        shutting_down: shutting_down.clone(),
        shutdown_rx: sessions_shutdown_rx,
    };
    let app = build_app(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await;
    });

    Ok(ServerHandle {
        addr,
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        shutting_down,
        shutdown_broadcast,
        shutdown_grace,
    })
}

fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .route("/metrics", get(metrics))
        .route("/l2", get(l2_ws_handler))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    StatusCode::OK
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.shutting_down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

async fn version() -> impl IntoResponse {
    let version = std::env::var("AERO_L2_PROXY_VERSION")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let git_sha = std::env::var("AERO_L2_PROXY_GIT_SHA")
        .or_else(|_| std::env::var("GIT_SHA"))
        .unwrap_or_else(|_| "dev".to_string());
    let built_at = std::env::var("AERO_L2_PROXY_BUILD_TIMESTAMP")
        .or_else(|_| std::env::var("BUILD_TIMESTAMP"))
        .unwrap_or_default();

    // Keep dependencies minimal: emit a tiny JSON object without pulling in serde/serde_json.
    fn escape_json_string(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        for ch in input.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    let body = format!(
        "{{\"version\":\"{}\",\"gitSha\":\"{}\",\"builtAt\":\"{}\"}}",
        escape_json_string(&version),
        escape_json_string(&git_sha),
        escape_json_string(&built_at)
    );

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        body,
    )
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let body = state.metrics.render_prometheus();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

async fn l2_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    ConnectInfo(client_addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let client_ip = client_addr.ip();

    if state.shutting_down.load(Ordering::SeqCst) {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response();
    }
    if !has_subprotocol(&headers, TUNNEL_SUBPROTOCOL) {
        return (
            StatusCode::BAD_REQUEST,
            format!("missing required websocket subprotocol {TUNNEL_SUBPROTOCOL:?}"),
        )
            .into_response();
    }

    if let Err(resp) = enforce_security(&state, &headers, &uri, client_ip) {
        return *resp;
    }

    let permit = match &state.connections {
        None => None,
        Some(semaphore) => match semaphore.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                state.metrics.upgrade_reject_max_connections();
                tracing::info!(
                    reason = "max_connections_exceeded",
                    origin = %origin_from_headers(&headers).unwrap_or("<missing>"),
                    auth_mode = %auth_mode(&state),
                    token_present = token_present(state.cfg.security.auth_mode, &headers, &uri),
                    cookie_present = session_cookie_present(&headers),
                    client_ip = %client_ip,
                    "rejected l2 websocket upgrade",
                );
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "max connections exceeded".to_string(),
                )
                    .into_response();
            }
        },
    };

    ws.protocols([TUNNEL_SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
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
    client_ip: std::net::IpAddr,
) -> Result<(), Box<axum::response::Response>> {
    // Auth is enforced before Origin checks so callers get a consistent 401 response when missing
    // credentials, even if the request is also missing/invalid Origin (see tests/security.rs).
    let token_present = token_present(state.cfg.security.auth_mode, headers, uri);
    let cookie_present = session_cookie_present(headers);
    match state.cfg.security.auth_mode {
        crate::config::AuthMode::None => {}
        crate::config::AuthMode::ApiKey => {
            let expected = state.cfg.security.api_key.as_deref().unwrap_or_default();
            let provided = query_param(uri, "apiKey")
                .or_else(|| query_param(uri, "token"))
                .or_else(|| token_from_subprotocol(headers));
            if provided.as_deref() != Some(expected) {
                if token_present {
                    state.metrics.upgrade_reject_auth_invalid();
                } else {
                    state.metrics.upgrade_reject_auth_missing();
                }
                tracing::warn!(
                    reason = if token_present {
                        "auth_invalid"
                    } else {
                        "auth_missing"
                    },
                    origin = %origin_from_headers(headers).unwrap_or("<missing>"),
                    auth_mode = %auth_mode(state),
                    token_present,
                    cookie_present,
                    client_ip = %client_ip,
                    "rejected l2 websocket upgrade",
                );
                return Err(Box::new(
                    (StatusCode::UNAUTHORIZED, "invalid api key".to_string()).into_response(),
                ));
            }
        }
        crate::config::AuthMode::Cookie => {
            let secret = state
                .cfg
                .security
                .session_secret
                .as_deref()
                .unwrap_or_default();
            let cookie = headers
                .get(axum::http::header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let session_token = cookie_value(cookie, SESSION_COOKIE_NAME);
            let cookie_present = session_token.is_some();
            let ok = session_token
                .as_deref()
                .and_then(|token| verify_session_token(token, secret))
                .is_some();
            if !ok {
                if cookie_present {
                    state.metrics.upgrade_reject_auth_invalid();
                } else {
                    state.metrics.upgrade_reject_auth_missing();
                }
                tracing::warn!(
                    reason = if cookie_present {
                        "auth_invalid"
                    } else {
                        "auth_missing"
                    },
                    origin = %origin_from_headers(headers).unwrap_or("<missing>"),
                    auth_mode = %auth_mode(state),
                    token_present,
                    cookie_present,
                    client_ip = %client_ip,
                    "rejected l2 websocket upgrade",
                );
                return Err(Box::new(
                    (
                        StatusCode::UNAUTHORIZED,
                        "missing or expired session".to_string(),
                    )
                        .into_response(),
                ));
            }
        }
        crate::config::AuthMode::Jwt => {
            let secret = state.cfg.security.jwt_secret.as_deref().unwrap_or_default();
            let token = query_param(uri, "token").or_else(|| token_from_subprotocol(headers));
            let token_present = token.is_some();
            let ok = token
                .as_deref()
                .is_some_and(|token| verify_jwt(token, secret));
            if !ok {
                if token_present {
                    state.metrics.upgrade_reject_auth_invalid();
                } else {
                    state.metrics.upgrade_reject_auth_missing();
                }
                tracing::warn!(
                    reason = if token_present {
                        "auth_invalid"
                    } else {
                        "auth_missing"
                    },
                    origin = %origin_from_headers(headers).unwrap_or("<missing>"),
                    auth_mode = %auth_mode(state),
                    token_present,
                    cookie_present,
                    client_ip = %client_ip,
                    "rejected l2 websocket upgrade",
                );
                return Err(Box::new(
                    (StatusCode::UNAUTHORIZED, "invalid jwt".to_string()).into_response(),
                ));
            }
        }
        crate::config::AuthMode::CookieOrJwt => {
            let cookie_secret = state
                .cfg
                .security
                .session_secret
                .as_deref()
                .unwrap_or_default();
            let jwt_secret = state.cfg.security.jwt_secret.as_deref().unwrap_or_default();

            let cookie = headers
                .get(axum::http::header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let session_token = cookie_value(cookie, SESSION_COOKIE_NAME);
            let cookie_present = session_token.is_some();
            let cookie_ok = session_token
                .as_deref()
                .and_then(|token| verify_session_token(token, cookie_secret))
                .is_some();
            if !cookie_ok {
                let token = query_param(uri, "token").or_else(|| token_from_subprotocol(headers));
                let token_present = token.is_some();
                let jwt_ok = token
                    .as_deref()
                    .is_some_and(|token| verify_jwt(token, jwt_secret));
                if !jwt_ok {
                    if cookie_present || token_present {
                        state.metrics.upgrade_reject_auth_invalid();
                    } else {
                        state.metrics.upgrade_reject_auth_missing();
                    }
                    tracing::warn!(
                        reason = if cookie_present || token_present {
                            "auth_invalid"
                        } else {
                            "auth_missing"
                        },
                        origin = %origin_from_headers(headers).unwrap_or("<missing>"),
                        auth_mode = %auth_mode(state),
                        token_present,
                        cookie_present,
                        client_ip = %client_ip,
                        "rejected l2 websocket upgrade",
                    );
                    return Err(Box::new(
                        (
                            StatusCode::UNAUTHORIZED,
                            "missing or invalid auth".to_string(),
                        )
                            .into_response(),
                    ));
                }
            }
        }
    }

    if !state.cfg.security.open {
        let origin_header = origin_from_headers(headers);
        let Some(origin_header) = origin_header else {
            state.metrics.upgrade_reject_origin_missing();
            tracing::warn!(
                reason = "origin_missing",
                origin = "<missing>",
                auth_mode = %auth_mode(state),
                token_present,
                cookie_present,
                client_ip = %client_ip,
                "rejected l2 websocket upgrade",
            );
            return Err(Box::new(
                (StatusCode::FORBIDDEN, "missing Origin header".to_string()).into_response(),
            ));
        };

        let origin = match crate::origin::normalize_origin(origin_header) {
            Some(origin) => origin,
            None => {
                state.metrics.upgrade_reject_origin_not_allowed();
                tracing::warn!(
                    reason = "origin_invalid",
                    origin = %origin_header,
                    auth_mode = %auth_mode(state),
                    token_present,
                    cookie_present,
                    client_ip = %client_ip,
                    "rejected l2 websocket upgrade",
                );
                return Err(Box::new(
                    (
                        StatusCode::FORBIDDEN,
                        format!("invalid Origin header: {origin_header}"),
                    )
                        .into_response(),
                ));
            }
        };

        match &state.cfg.security.allowed_origins {
            crate::config::AllowedOrigins::Any => {}
            crate::config::AllowedOrigins::List(list) => {
                if !list.iter().any(|allowed| allowed == &origin) {
                    state.metrics.upgrade_reject_origin_not_allowed();
                    tracing::warn!(
                        reason = "origin_not_allowed",
                        origin = %origin,
                        auth_mode = %auth_mode(state),
                        token_present,
                        cookie_present,
                        client_ip = %client_ip,
                        "rejected l2 websocket upgrade",
                    );
                    return Err(Box::new(
                        (
                            StatusCode::FORBIDDEN,
                            format!("Origin not allowed: {origin}"),
                        )
                            .into_response(),
                    ));
                }
            }
        }
    }

    Ok(())
}

fn auth_mode(state: &AppState) -> &'static str {
    match state.cfg.security.auth_mode {
        crate::config::AuthMode::None => "none",
        crate::config::AuthMode::Cookie => "cookie",
        crate::config::AuthMode::ApiKey => "api_key",
        crate::config::AuthMode::Jwt => "jwt",
        crate::config::AuthMode::CookieOrJwt => "cookie_or_jwt",
    }
}

fn origin_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn token_present(
    auth_mode: crate::config::AuthMode,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
) -> bool {
    match auth_mode {
        crate::config::AuthMode::ApiKey => {
            token_present_in_query(uri, "apiKey")
                || token_present_in_query(uri, "token")
                || token_present_in_subprotocol(headers)
        }
        _ => token_present_in_query(uri, "token") || token_present_in_subprotocol(headers),
    }
}

fn token_present_in_query(uri: &axum::http::Uri, key: &str) -> bool {
    let Some(query) = uri.query() else {
        return false;
    };
    for part in query.split('&') {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        if k == key {
            return !v.is_empty();
        }
    }
    false
}

fn token_present_in_subprotocol(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };

    value.split(',').map(str::trim).any(|proto| {
        proto
            .strip_prefix("aero-l2-token.")
            .is_some_and(|v| !v.is_empty())
    })
}

fn session_cookie_present(headers: &HeaderMap) -> bool {
    let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };

    cookie.split(';').any(|part| {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return false;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            return false;
        };
        k.trim() == SESSION_COOKIE_NAME && !v.trim().is_empty()
    })
}

fn query_param(uri: &axum::http::Uri, key: &str) -> Option<String> {
    let query = uri.query()?;
    for part in query.split('&') {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        if k == key {
            return (!v.is_empty()).then(|| percent_decode(v));
        }
    }
    None
}

fn cookie_value(cookie_header: &str, key: &str) -> Option<String> {
    for part in cookie_header.split(';') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (k, v) = trimmed.split_once('=')?;
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        return (!v.is_empty()).then(|| percent_decode(v));
    }
    None
}

fn verify_session_token(token: &str, secret: &[u8]) -> Option<String> {
    let (payload_b64, sig_b64) = token.split_once('.')?;

    let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sig_b64.as_bytes())
        .ok()?;

    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::verify(&key, payload_b64.as_bytes(), &sig).ok()?;

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .ok()?;

    let payload: SessionTokenPayload = serde_json::from_slice(&payload_bytes).ok()?;
    if payload.v != 1 || payload.sid.trim().is_empty() {
        return None;
    }

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|dur| dur.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let expires_at_ms = payload.exp.saturating_mul(1000);
    if expires_at_ms <= now_ms {
        return None;
    }

    Some(payload.sid)
}

fn verify_jwt(token: &str, secret: &[u8]) -> bool {
    let mut parts = token.split('.');
    let Some(header_b64) = parts.next() else {
        return false;
    };
    let Some(payload_b64) = parts.next() else {
        return false;
    };
    let Some(sig_b64) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }

    let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sig_b64.as_bytes())
        .ok();
    let Some(sig) = sig else {
        return false;
    };

    let signing_input = format!("{header_b64}.{payload_b64}");
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    if hmac::verify(&key, signing_input.as_bytes(), &sig).is_err() {
        return false;
    }

    // Best-effort `exp` check when present.
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .ok();
    let Some(payload_bytes) = payload_bytes else {
        return true;
    };
    let payload: serde_json::Value = match serde_json::from_slice(&payload_bytes) {
        Ok(v) => v,
        Err(_) => return true,
    };
    let Some(exp) = payload.get("exp").and_then(|v| v.as_u64()) else {
        return true;
    };

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|dur| dur.as_secs())
        .unwrap_or(0);
    exp > now_secs
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
    let value = headers
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())?;

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
