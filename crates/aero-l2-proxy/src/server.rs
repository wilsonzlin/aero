use std::{
    borrow::Cow,
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
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

use crate::{
    capture::CaptureManager,
    dns::DnsService,
    gateway_session,
    metrics::{AuthRejectReason, Metrics},
    session,
    session_limits::{SessionTunnelPermit, SessionTunnelTracker},
    ProxyConfig, TUNNEL_SUBPROTOCOL,
};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) cfg: Arc<ProxyConfig>,
    pub(crate) dns: Arc<DnsService>,
    pub(crate) l2_limits: aero_l2_protocol::Limits,
    pub(crate) metrics: Metrics,
    pub(crate) capture: CaptureManager,
    pub(crate) connections: Option<Arc<Semaphore>>,
    connections_per_ip: Option<Arc<IpConnectionLimiter>>,
    pub(crate) session_tunnels: Option<Arc<SessionTunnelTracker>>,
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
        cfg.dns_lookup_timeout,
    )
    .map_err(std::io::Error::other)?;

    let l2_limits = aero_l2_protocol::Limits {
        max_frame_payload: cfg.l2_max_frame_payload,
        max_control_payload: cfg.l2_max_control_payload,
    };

    let metrics = Metrics::new();
    let capture = CaptureManager::new(
        cfg.capture_dir.clone(),
        cfg.capture_max_bytes,
        cfg.capture_flush_interval,
        metrics.clone(),
    )
    .await;

    let connections = (cfg.security.max_connections != 0)
        .then(|| Arc::new(Semaphore::new(cfg.security.max_connections)));
    let connections_per_ip = (cfg.security.max_connections_per_ip != 0).then(|| {
        Arc::new(IpConnectionLimiter::new(
            cfg.security.max_connections_per_ip,
        ))
    });

    let session_tunnels = (cfg.security.max_tunnels_per_session != 0).then(|| {
        Arc::new(SessionTunnelTracker::new(
            cfg.security.max_tunnels_per_session,
        ))
    });

    let shutting_down = Arc::new(AtomicBool::new(false));
    let (shutdown_broadcast, sessions_shutdown_rx) = watch::channel(false);

    let state = AppState {
        cfg: Arc::new(cfg),
        dns: Arc::new(dns),
        l2_limits,
        metrics,
        capture,
        connections,
        connections_per_ip,
        session_tunnels,
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
        .route("/l2/", get(l2_ws_handler))
        // Legacy alias (see `docs/l2-tunnel-protocol.md`).
        .route("/eth", get(l2_ws_handler))
        .route("/eth/", get(l2_ws_handler))
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
        .or_else(|_| std::env::var("VERSION"))
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let git_sha = std::env::var("AERO_L2_PROXY_GIT_SHA")
        .or_else(|_| std::env::var("GIT_SHA"))
        .unwrap_or_else(|_| "dev".to_string());
    let built_at = std::env::var("AERO_L2_PROXY_BUILD_TIMESTAMP")
        .or_else(|_| std::env::var("BUILD_TIMESTAMP"))
        .unwrap_or_default();

    // Emit a tiny JSON object manually so the endpoint stays stable and doesn't require a
    // dedicated response struct.
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
        [(axum::http::header::CONTENT_TYPE, "application/json")],
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
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> impl IntoResponse {
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

    let (client_ip, client_ip_source) =
        derive_client_ip(state.cfg.security.trust_proxy, &headers, connect_info);

    let session_identity = match enforce_security(&state, &headers, &uri, client_ip) {
        Ok(session_identity) => session_identity,
        Err(resp) => return *resp,
    };

    let ip_permit = match &state.connections_per_ip {
        None => None,
        Some(limiter) => match limiter.try_acquire(client_ip) {
            Ok(permit) => Some(permit),
            Err(IpLimitExceeded { limit, active }) => {
                state.metrics.upgrade_ip_limit_exceeded();
                tracing::warn!(
                    client_ip = %client_ip,
                    client_ip_source = ?client_ip_source,
                    max_connections_per_ip = limit,
                    active_connections = active,
                    "rejecting l2 upgrade: per-IP connection limit exceeded",
                );
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "max connections per ip exceeded".to_string(),
                )
                    .into_response();
            }
        },
    };

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
                    auth_sid = session_identity.as_deref().unwrap_or("none"),
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

    let session_tunnel_permit: Option<SessionTunnelPermit> =
        match (session_identity.as_deref(), state.session_tunnels.as_ref()) {
            (Some(session_id), Some(tracker)) => tracker.try_acquire(session_id),
            _ => None,
        };

    if session_identity.is_some()
        && state.session_tunnels.is_some()
        && session_tunnel_permit.is_none()
    {
        state.metrics.upgrade_reject_max_connections_per_session();
        state.metrics.session_connection_denied();
        tracing::warn!(
            reason = "max_connections_per_session_exceeded",
            origin = %origin_from_headers(&headers).unwrap_or("<missing>"),
            auth_mode = %auth_mode(&state),
            auth_sid = session_identity.as_deref().unwrap_or("none"),
            token_present = token_present(state.cfg.security.auth_mode, &headers, &uri),
            cookie_present = session_cookie_present(&headers),
            client_ip = %client_ip,
            "rejected l2 websocket upgrade",
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "quota_connections: max connections per session exceeded".to_string(),
        )
            .into_response();
    }

    let max_payload = state
        .l2_limits
        .max_frame_payload
        .max(state.l2_limits.max_control_payload);
    let max_ws_message_size = aero_l2_protocol::L2_TUNNEL_HEADER_LEN.saturating_add(max_payload);

    ws.protocols([TUNNEL_SUBPROTOCOL])
        .max_message_size(max_ws_message_size)
        .max_frame_size(max_ws_message_size)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            let _ip_permit = ip_permit;
            let _session_tunnel_permit = session_tunnel_permit;
            handle_l2_ws(socket, state, session_identity).await;
        })
}

async fn handle_l2_ws(socket: WebSocket, state: AppState, session_identity: Option<String>) {
    let tunnel_id = state.metrics.next_session_id();
    if let Err(err) = session::run_session(socket, state, tunnel_id, session_identity).await {
        tracing::debug!(tunnel_id, "l2 session ended: {err:#}");
    }
}

fn reject_auth_unauthorized(
    state: &AppState,
    headers: &HeaderMap,
    token_present: bool,
    cookie_present: bool,
    client_ip: IpAddr,
    reject_reason: AuthRejectReason,
    message: &'static str,
) -> Box<axum::response::Response> {
    let missing = matches!(reject_reason, AuthRejectReason::MissingCredentials);
    let reason = if missing { "auth_missing" } else { "auth_invalid" };
    reject_auth_unauthorized_with_reason(
        state,
        headers,
        token_present,
        cookie_present,
        client_ip,
        reject_reason,
        reason,
        message,
    )
}

fn reject_auth_unauthorized_with_reason(
    state: &AppState,
    headers: &HeaderMap,
    token_present: bool,
    cookie_present: bool,
    client_ip: IpAddr,
    reject_reason: AuthRejectReason,
    reason: &'static str,
    message: &'static str,
) -> Box<axum::response::Response> {
    state.metrics.auth_failed();
    if matches!(reject_reason, AuthRejectReason::MissingCredentials) {
        state.metrics.upgrade_reject_auth_missing();
    } else {
        state.metrics.upgrade_reject_auth_invalid();
    }
    state.metrics.auth_rejected(reject_reason);
    tracing::warn!(
        reason,
        auth_reject_reason = reject_reason.label(),
        origin = %origin_from_headers(headers).unwrap_or("<missing>"),
        auth_mode = %auth_mode(state),
        token_present,
        cookie_present,
        client_ip = %client_ip,
        "rejected l2 websocket upgrade",
    );
    Box::new((StatusCode::UNAUTHORIZED, message.to_string()).into_response())
}

fn reject_invalid_jwt(
    state: &AppState,
    headers: &HeaderMap,
    token_present: bool,
    cookie_present: bool,
    client_ip: IpAddr,
    context: &'static str,
) -> Box<axum::response::Response> {
    state.metrics.auth_failed();
    state.metrics.upgrade_reject_auth_invalid();
    state.metrics.auth_rejected(AuthRejectReason::InvalidJwt);
    tracing::warn!(
        reason = "auth_invalid",
        auth_reject_reason = AuthRejectReason::InvalidJwt.label(),
        origin = %origin_from_headers(headers).unwrap_or("<missing>"),
        auth_mode = %auth_mode(state),
        token_present,
        cookie_present,
        client_ip = %client_ip,
        "rejected l2 websocket upgrade ({context})",
    );
    Box::new((StatusCode::UNAUTHORIZED, "invalid jwt".to_string()).into_response())
}

fn session_cookie_raw_value<'a>(headers: &'a HeaderMap) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .find_map(gateway_session::extract_session_cookie_raw_value)
}

fn session_id_from_cookie(headers: &HeaderMap, secret: &[u8], now_ms: u64) -> Option<String> {
    let raw = session_cookie_raw_value(headers)?;
    let token = if raw.contains('%') {
        Cow::Owned(gateway_session::percent_decode(raw))
    } else {
        Cow::Borrowed(raw)
    };
    gateway_session::verify_session_token(token.as_ref(), secret, now_ms).map(|session| session.id)
}

fn reject_origin_forbidden(
    state: &AppState,
    _headers: &HeaderMap,
    token_present: bool,
    cookie_present: bool,
    client_ip: IpAddr,
    reason: &'static str,
    origin: &str,
    message: String,
    record_metric: impl FnOnce(&Metrics),
) -> Box<axum::response::Response> {
    record_metric(&state.metrics);
    tracing::warn!(
        reason,
        origin = %origin,
        auth_mode = %auth_mode(state),
        token_present,
        cookie_present,
        client_ip = %client_ip,
        "rejected l2 websocket upgrade",
    );
    Box::new((StatusCode::FORBIDDEN, message).into_response())
}

fn reject_host_forbidden(
    state: &AppState,
    headers: &HeaderMap,
    token_present: bool,
    cookie_present: bool,
    client_ip: IpAddr,
    reason: &'static str,
    host: &str,
    message: String,
    record_metric: impl FnOnce(&Metrics),
) -> Box<axum::response::Response> {
    record_metric(&state.metrics);
    tracing::warn!(
        reason,
        origin = %origin_from_headers(headers).unwrap_or("<missing>"),
        auth_mode = %auth_mode(state),
        token_present,
        cookie_present,
        client_ip = %client_ip,
        host = %host,
        "rejected l2 websocket upgrade",
    );
    Box::new((StatusCode::FORBIDDEN, message).into_response())
}

fn enforce_security(
    state: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    client_ip: std::net::IpAddr,
) -> Result<Option<String>, Box<axum::response::Response>> {
    // Auth is enforced before Origin checks so callers get a consistent 401 response when missing
    // credentials, even if the request is also missing/invalid Origin (see tests/security.rs).
    let token_present = token_present(state.cfg.security.auth_mode, headers, uri);
    let cookie_present = session_cookie_present(headers);
    let origin_normalized_for_jwt =
        origin_from_headers(headers).and_then(crate::origin::normalize_origin);

    let mut auth_sid: Option<String> = None;
    match state.cfg.security.auth_mode {
        crate::config::AuthMode::None => {}
        crate::config::AuthMode::ApiKey => {
            let expected = state.cfg.security.api_key.as_deref().unwrap_or_default();
            let provided = token_from_query(uri)
                .or_else(|| token_from_subprotocol(headers).map(Cow::Borrowed));
            let api_key_ok = provided
                .as_deref()
                .is_some_and(|provided| constant_time_eq(provided, expected));
            if !api_key_ok {
                let reject_reason = if token_present {
                    AuthRejectReason::InvalidApiKey
                } else {
                    AuthRejectReason::MissingCredentials
                };
                return Err(reject_auth_unauthorized(
                    state,
                    headers,
                    token_present,
                    cookie_present,
                    client_ip,
                    reject_reason,
                    "invalid token",
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
            let now_ms = now_ms();
            let sid = session_id_from_cookie(headers, secret, now_ms);
            if sid.is_none() {
                let reject_reason = if cookie_present {
                    AuthRejectReason::InvalidCookie
                } else {
                    AuthRejectReason::MissingCredentials
                };
                return Err(reject_auth_unauthorized(
                    state,
                    headers,
                    token_present,
                    cookie_present,
                    client_ip,
                    reject_reason,
                    "missing or expired session",
                ));
            }
            auth_sid = sid;
        }
        crate::config::AuthMode::Jwt => {
            let (sid, expected_origin) = verify_jwt_sid(
                state,
                headers,
                uri,
                &origin_normalized_for_jwt,
                token_present,
                cookie_present,
                client_ip,
            )?;
            auth_sid = Some(sid);
            if let Some(expected_origin) = expected_origin {
                if origin_claim_mismatch(
                    state.cfg.security.open,
                    origin_normalized_for_jwt.as_deref(),
                    &expected_origin,
                ) {
                    reject_jwt_origin_mismatch(
                        state,
                        headers,
                        token_present,
                        cookie_present,
                        client_ip,
                    )?;
                }
            }
        }
        crate::config::AuthMode::CookieOrJwt => {
            let cookie_secret = state
                .cfg
                .security
                .session_secret
                .as_deref()
                .unwrap_or_default();

            let now_ms = now_ms();
            let sid = session_id_from_cookie(headers, cookie_secret, now_ms);

            if let Some(sid) = sid {
                auth_sid = Some(sid);
            } else {
                if !token_present {
                    let reject_reason = if cookie_present {
                        AuthRejectReason::InvalidCookie
                    } else {
                        AuthRejectReason::MissingCredentials
                    };
                    return Err(reject_auth_unauthorized(
                        state,
                        headers,
                        token_present,
                        cookie_present,
                        client_ip,
                        reject_reason,
                        "missing or invalid auth",
                    ));
                }
                let (sid, expected_origin) = verify_jwt_sid(
                    state,
                    headers,
                    uri,
                    &origin_normalized_for_jwt,
                    token_present,
                    cookie_present,
                    client_ip,
                )?;
                auth_sid = Some(sid);
                if let Some(expected_origin) = expected_origin {
                    if origin_claim_mismatch(
                        state.cfg.security.open,
                        origin_normalized_for_jwt.as_deref(),
                        &expected_origin,
                    ) {
                        reject_jwt_origin_mismatch(
                            state,
                            headers,
                            token_present,
                            cookie_present,
                            client_ip,
                        )?;
                    }
                }
            }
        }
        crate::config::AuthMode::CookieOrApiKey => {
            let cookie_secret = state
                .cfg
                .security
                .session_secret
                .as_deref()
                .unwrap_or_default();
            let expected = state.cfg.security.api_key.as_deref().unwrap_or_default();

            let now_ms = now_ms();
            let sid = session_id_from_cookie(headers, cookie_secret, now_ms);

            if let Some(sid) = sid {
                auth_sid = Some(sid);
            } else {
                let provided = token_from_query(uri)
                    .or_else(|| token_from_subprotocol(headers).map(Cow::Borrowed));
                let api_key_present = provided.is_some();
                let api_key_ok = provided
                    .as_deref()
                    .is_some_and(|provided| constant_time_eq(provided, expected));

                if !api_key_ok {
                    let reject_reason = if api_key_present {
                        AuthRejectReason::InvalidApiKey
                    } else if cookie_present {
                        AuthRejectReason::InvalidCookie
                    } else {
                        AuthRejectReason::MissingCredentials
                    };
                    return Err(reject_auth_unauthorized(
                        state,
                        headers,
                        token_present,
                        cookie_present,
                        client_ip,
                        reject_reason,
                        "missing or invalid auth",
                    ));
                }
            }
        }
        crate::config::AuthMode::CookieAndApiKey => {
            let cookie_secret = state
                .cfg
                .security
                .session_secret
                .as_deref()
                .unwrap_or_default();
            let expected = state.cfg.security.api_key.as_deref().unwrap_or_default();

            let now_ms = now_ms();
            let sid = session_id_from_cookie(headers, cookie_secret, now_ms);
            let cookie_ok = sid.is_some();

            let provided = token_from_query(uri)
                .or_else(|| token_from_subprotocol(headers).map(Cow::Borrowed));
            let api_key_present = provided.is_some();
            let api_key_ok = provided
                .as_deref()
                .is_some_and(|provided| constant_time_eq(provided, expected));

            if !cookie_ok || !api_key_ok {
                let reject_reason = if api_key_present && !api_key_ok {
                    AuthRejectReason::InvalidApiKey
                } else if cookie_present && !cookie_ok {
                    AuthRejectReason::InvalidCookie
                } else {
                    AuthRejectReason::MissingCredentials
                };
                return Err(reject_auth_unauthorized(
                    state,
                    headers,
                    token_present,
                    cookie_present,
                    client_ip,
                    reject_reason,
                    "missing or invalid auth",
                ));
            }

            auth_sid = sid;
        }
    }

    if !state.cfg.security.open {
        let mut origin_values = headers.get_all(axum::http::header::ORIGIN).iter();
        let Some(origin_header) = origin_values.next() else {
            return Err(reject_origin_forbidden(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "origin_missing",
                "<missing>",
                "missing Origin header".to_string(),
                |m| m.upgrade_reject_origin_missing(),
            ));
        };
        if origin_values.next().is_some() {
            return Err(reject_origin_forbidden(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "origin_invalid",
                "<multiple>",
                "invalid Origin header: multiple values".to_string(),
                |m| m.upgrade_reject_origin_not_allowed(),
            ));
        }

        let origin_header = origin_header
            .to_str()
            .ok()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let Some(origin_header) = origin_header else {
            return Err(reject_origin_forbidden(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "origin_missing",
                "<missing>",
                "missing Origin header".to_string(),
                |m| m.upgrade_reject_origin_missing(),
            ));
        };

        let origin = match crate::origin::normalize_origin(origin_header) {
            Some(origin) => origin,
            None => {
                return Err(reject_origin_forbidden(
                    state,
                    headers,
                    token_present,
                    cookie_present,
                    client_ip,
                    "origin_invalid",
                    origin_header,
                    format!("invalid Origin header: {origin_header}"),
                    |m| m.upgrade_reject_origin_not_allowed(),
                ));
            }
        };

        match &state.cfg.security.allowed_origins {
            crate::config::AllowedOrigins::Any => {}
            crate::config::AllowedOrigins::List(list) => {
                let allowed = if list.is_empty() {
                    let request_host = headers
                        .get(header::HOST)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    crate::origin::is_origin_allowed(origin_header, request_host, list)
                } else {
                    list.iter().any(|allowed| allowed == &origin)
                };

                if !allowed {
                    return Err(reject_origin_forbidden(
                        state,
                        headers,
                        token_present,
                        cookie_present,
                        client_ip,
                        "origin_not_allowed",
                        &origin,
                        format!("Origin not allowed: {origin}"),
                        |m| m.upgrade_reject_origin_not_allowed(),
                    ));
                }
            }
        }
    }

    if !state.cfg.security.allowed_hosts.is_empty() {
        let trust_proxy = state.cfg.security.trust_proxy_host;
        let scheme = effective_host_scheme(headers, trust_proxy);
        let raw_host = effective_host_value(headers, trust_proxy);
        let Some(raw_host) = raw_host else {
            return Err(reject_host_forbidden(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "host_missing",
                "<missing>",
                "missing Host header".to_string(),
                |m| m.upgrade_reject_host_missing(),
            ));
        };

        let Some(host) = normalize_host_for_compare(&raw_host, scheme) else {
            return Err(reject_host_forbidden(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "host_invalid",
                &raw_host,
                "malformed Host header".to_string(),
                |m| m.upgrade_reject_host_invalid(),
            ));
        };

        let is_allowed = state.cfg.security.allowed_hosts.iter().any(|allowed| {
            normalize_host_for_compare(allowed, scheme).is_some_and(|allowed| allowed == host)
        });

        if !is_allowed {
            return Err(reject_host_forbidden(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "host_not_allowed",
                &host,
                format!("Host not allowed: {host}"),
                |m| m.upgrade_reject_host_not_allowed(),
            ));
        }
    }

    Ok(auth_sid)
}

fn verify_jwt_sid(
    state: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    _origin_normalized_for_jwt: &Option<String>,
    token_present: bool,
    cookie_present: bool,
    client_ip: IpAddr,
) -> Result<(String, Option<String>), Box<axum::response::Response>> {
    let secret = state.cfg.security.jwt_secret.as_deref().unwrap_or_default();
    let now_unix = now_unix_seconds();
    let claims = bearer_token(headers)
        .and_then(|token| crate::auth::verify_relay_jwt_hs256(token, secret, now_unix).ok())
        .or_else(|| {
            token_from_query(uri)
                .and_then(|token| crate::auth::verify_relay_jwt_hs256(token.as_ref(), secret, now_unix).ok())
        })
        .or_else(|| {
            token_from_subprotocol(headers)
                .and_then(|token| crate::auth::verify_relay_jwt_hs256(token, secret, now_unix).ok())
        });

    let claims = match claims {
        Some(claims) => claims,
        None => {
            let reject_reason = if token_present {
                AuthRejectReason::InvalidJwt
            } else {
                AuthRejectReason::MissingCredentials
            };
            return Err(reject_auth_unauthorized(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                reject_reason,
                "invalid jwt",
            ));
        }
    };

    if let Some(expected) = state.cfg.security.jwt_audience.as_deref() {
        if claims.aud.as_deref() != Some(expected) {
            return Err(reject_invalid_jwt(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "jwt audience mismatch",
            ));
        }
    }

    if let Some(expected) = state.cfg.security.jwt_issuer.as_deref() {
        if claims.iss.as_deref() != Some(expected) {
            return Err(reject_invalid_jwt(
                state,
                headers,
                token_present,
                cookie_present,
                client_ip,
                "jwt issuer mismatch",
            ));
        }
    }

    let expected_origin = match claims.origin.as_deref() {
        None => None,
        Some(raw) => match crate::origin::normalize_origin(raw) {
            Some(origin) => Some(origin),
            None => {
                return Err(reject_invalid_jwt(
                    state,
                    headers,
                    token_present,
                    cookie_present,
                    client_ip,
                    "jwt origin claim invalid",
                ));
            }
        },
    };

    Ok((claims.sid, expected_origin))
}

fn origin_claim_mismatch(open: bool, actual: Option<&str>, expected: &str) -> bool {
    match actual {
        Some(actual) => actual != expected,
        None => open,
    }
}

fn reject_jwt_origin_mismatch(
    state: &AppState,
    headers: &HeaderMap,
    token_present: bool,
    cookie_present: bool,
    client_ip: IpAddr,
) -> Result<(), Box<axum::response::Response>> {
    Err(reject_auth_unauthorized_with_reason(
        state,
        headers,
        token_present,
        cookie_present,
        client_ip,
        AuthRejectReason::JwtOriginMismatch,
        "jwt_origin_mismatch",
        "invalid jwt",
    ))
}

fn auth_mode(state: &AppState) -> &'static str {
    match state.cfg.security.auth_mode {
        crate::config::AuthMode::None => "none",
        // Prefer the canonical auth mode spellings used in docs/config.
        // (The config parser still accepts legacy aliases like `cookie`/`api_key`.)
        crate::config::AuthMode::Cookie => "session",
        crate::config::AuthMode::ApiKey => "token",
        crate::config::AuthMode::Jwt => "jwt",
        crate::config::AuthMode::CookieOrJwt => "cookie_or_jwt",
        crate::config::AuthMode::CookieOrApiKey => "session_or_token",
        crate::config::AuthMode::CookieAndApiKey => "session_and_token",
    }
}

fn origin_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Copy, Clone)]
enum HostScheme {
    Http,
    Https,
}

impl HostScheme {
    fn default_port(self) -> u16 {
        match self {
            HostScheme::Http => 80,
            HostScheme::Https => 443,
        }
    }
}

fn effective_host_scheme(headers: &HeaderMap, trust_proxy: bool) -> HostScheme {
    if trust_proxy {
        if let Some(proto) = forwarded_param(headers, "proto")
            .or_else(|| header_list_value(headers, "x-forwarded-proto"))
        {
            let proto = proto.to_ascii_lowercase();
            if matches!(proto.as_str(), "https" | "wss") {
                return HostScheme::Https;
            }
            if matches!(proto.as_str(), "http" | "ws") {
                return HostScheme::Http;
            }
        }
    }

    // Fall back to Origin scheme when present (useful for browser clients when proxy proto headers
    // are not configured).
    if let Some(origin) = origin_from_headers(headers) {
        let origin = origin.to_ascii_lowercase();
        if origin.starts_with("https://") || origin.starts_with("wss://") {
            return HostScheme::Https;
        }
        if origin.starts_with("http://") || origin.starts_with("ws://") {
            return HostScheme::Http;
        }
    }

    HostScheme::Http
}

fn effective_host_value(headers: &HeaderMap, trust_proxy: bool) -> Option<String> {
    if trust_proxy {
        if let Some(host) = forwarded_param(headers, "host") {
            return Some(host);
        }
        if let Some(host) = header_list_value(headers, "x-forwarded-host") {
            return Some(host);
        }
    }

    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string())
}

fn normalize_host_for_compare(raw: &str, scheme: HostScheme) -> Option<String> {
    let raw = raw.split(',').next().unwrap_or("").trim();
    if raw.is_empty() {
        return None;
    }

    let raw = raw
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(raw);

    // Reject whitespace anywhere in the host value; if it exists, callers should treat it as
    // malformed.
    if raw.chars().any(|c| c.is_whitespace()) {
        return None;
    }

    let raw = raw.to_ascii_lowercase();

    if let Some(rest) = raw.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = format!("[{}]", &rest[..end]);
        let rest = &rest[end + 1..];
        if rest.is_empty() {
            return Some(host);
        }
        let port_str = rest.strip_prefix(':')?;
        let port = port_str.parse::<u16>().ok()?;
        if port == scheme.default_port() {
            return Some(host);
        }
        return Some(format!("{host}:{port}"));
    }

    if let Some((host, port_str)) = raw.rsplit_once(':') {
        if host.is_empty() || host.contains(':') {
            return None;
        }
        if port_str.is_empty() || !port_str.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let port = port_str.parse::<u16>().ok()?;
        if port == scheme.default_port() {
            return Some(host.to_string());
        }
        return Some(format!("{host}:{port}"));
    }

    Some(raw)
}

fn header_list_value(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
}

fn forwarded_param(headers: &HeaderMap, param: &str) -> Option<String> {
    let value = headers.get("forwarded")?.to_str().ok()?;
    let first = value.split(',').next()?.trim();
    for part in first.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=')?;
        if !k.trim().eq_ignore_ascii_case(param) {
            continue;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(v);
        if v.is_empty() {
            return None;
        }
        return Some(v.to_string());
    }
    None
}

fn token_present(
    auth_mode: crate::config::AuthMode,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
) -> bool {
    match auth_mode {
        crate::config::AuthMode::Jwt | crate::config::AuthMode::CookieOrJwt => {
            bearer_token(headers).is_some()
                || token_present_in_query(uri, "apiKey")
                || token_present_in_query(uri, "token")
                || token_present_in_subprotocol(headers)
        }
        crate::config::AuthMode::ApiKey
        | crate::config::AuthMode::CookieOrApiKey
        | crate::config::AuthMode::CookieAndApiKey => {
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
    token_from_subprotocol(headers).is_some()
}

fn session_cookie_present(headers: &HeaderMap) -> bool {
    // Match gateway semantics: "first cookie wins", and an empty cookie value is treated as missing.
    matches!(session_cookie_raw_value(headers), Some(v) if !v.is_empty())
}

fn query_param_raw<'a>(uri: &'a axum::http::Uri, key: &str) -> Option<&'a str> {
    let query = uri.query()?;
    for part in query.split('&') {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        if k == key {
            return (!v.is_empty()).then_some(v);
        }
    }
    None
}

fn query_param<'a>(uri: &'a axum::http::Uri, key: &str) -> Option<Cow<'a, str>> {
    let v = query_param_raw(uri, key)?;
    if v.contains('%') {
        return Some(Cow::Owned(gateway_session::percent_decode(v)));
    }
    Some(Cow::Borrowed(v))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let mut parts = value.split_whitespace();
    let scheme = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    (!token.is_empty()).then_some(token)
}

fn token_from_query(uri: &axum::http::Uri) -> Option<Cow<'_, str>> {
    query_param(uri, "token").or_else(|| query_param(uri, "apiKey"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|dur| dur.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|dur| dur.as_secs())
        .unwrap_or(0)
}

fn token_from_subprotocol(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())?;

    value.split(',').map(str::trim).find_map(|proto| {
        proto
            .strip_prefix(aero_l2_protocol::L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX)
            .filter(|v| !v.is_empty())
            .map(|v| v)
    })
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&a, &b) in a.as_bytes().iter().zip(b.as_bytes()) {
        diff |= a ^ b;
    }
    diff == 0
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

#[derive(Debug, Clone, Copy)]
enum ClientIpSource {
    ConnectInfo,
    Forwarded,
    XForwardedFor,
}

fn derive_client_ip(
    trust_proxy: bool,
    headers: &HeaderMap,
    connect_info: SocketAddr,
) -> (IpAddr, ClientIpSource) {
    let remote_ip = connect_info.ip();
    if !trust_proxy {
        return (remote_ip, ClientIpSource::ConnectInfo);
    }

    if let Some(ip) = client_ip_from_forwarded(headers) {
        return (ip, ClientIpSource::Forwarded);
    }

    if let Some(ip) = client_ip_from_x_forwarded_for(headers) {
        return (ip, ClientIpSource::XForwardedFor);
    }

    (remote_ip, ClientIpSource::ConnectInfo)
}

fn client_ip_from_forwarded(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get_all("forwarded")
        .iter()
        .find_map(|value| value.to_str().ok().and_then(parse_forwarded_for))
}

fn parse_forwarded_for(value: &str) -> Option<IpAddr> {
    for element in value.split(',') {
        for param in element.split(';') {
            let param = param.trim();
            if param.len() < 4 || !param[..4].eq_ignore_ascii_case("for=") {
                continue;
            }

            let mut raw = param[4..].trim();
            if let Some(stripped) = raw
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
            {
                raw = stripped;
            }

            if raw.is_empty() {
                continue;
            }

            if let Some(stripped) = raw.strip_prefix('[') {
                let Some(end) = stripped.find(']') else {
                    continue;
                };
                raw = &stripped[..end];
            }

            if let Ok(ip) = raw.parse::<IpAddr>() {
                return Some(ip);
            }

            if let Ok(addr) = raw.parse::<SocketAddr>() {
                return Some(addr.ip());
            }
        }
    }

    None
}

fn client_ip_from_x_forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .find_map(|value| value.to_str().ok().and_then(parse_x_forwarded_for))
}

fn parse_x_forwarded_for(value: &str) -> Option<IpAddr> {
    let first = value.split(',').next()?.trim();
    if first.is_empty() {
        return None;
    }

    if let Ok(ip) = first.parse::<IpAddr>() {
        return Some(ip);
    }

    first.parse::<SocketAddr>().ok().map(|addr| addr.ip())
}

pub(crate) struct IpConnectionLimiter {
    max: u32,
    counts: Mutex<HashMap<IpAddr, u32>>,
}

#[derive(Debug, Clone, Copy)]
struct IpLimitExceeded {
    limit: u32,
    active: u32,
}

impl IpConnectionLimiter {
    fn new(max: u32) -> Self {
        Self {
            max,
            counts: Mutex::new(HashMap::new()),
        }
    }

    fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Result<IpConnectionPermit, IpLimitExceeded> {
        let mut counts = self.counts.lock().unwrap_or_else(|err| err.into_inner());
        let active = counts.entry(ip).or_insert(0);
        if *active >= self.max {
            return Err(IpLimitExceeded {
                limit: self.max,
                active: *active,
            });
        }
        *active += 1;
        Ok(IpConnectionPermit {
            ip,
            limiter: Arc::clone(self),
        })
    }

    fn release(&self, ip: IpAddr) {
        let mut counts = self.counts.lock().unwrap_or_else(|err| err.into_inner());
        let Some(active) = counts.get_mut(&ip) else {
            return;
        };
        if *active <= 1 {
            counts.remove(&ip);
        } else {
            *active -= 1;
        }
    }
}

struct IpConnectionPermit {
    ip: IpAddr,
    limiter: Arc<IpConnectionLimiter>,
}

impl Drop for IpConnectionPermit {
    fn drop(&mut self) {
        self.limiter.release(self.ip);
    }
}
