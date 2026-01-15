#![cfg(not(target_arch = "wasm32"))]

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aero_l2_proxy::{auth, protocol, start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::{HeaderValue, StatusCode},
    Error as WsError, Message,
};

static ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct EnvVarGuard {
    key: &'static str,
    prior: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }

    fn unset(key: &'static str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prior }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct CommonL2Env {
    _max_connections: EnvVarGuard,
    _max_connections_per_session: EnvVarGuard,
    _max_bytes: EnvVarGuard,
    _max_fps: EnvVarGuard,
    _ping_interval: EnvVarGuard,
    _auth_mode: EnvVarGuard,
    _session_secret: EnvVarGuard,
    _session_secret_alias: EnvVarGuard,
    _gateway_session_secret: EnvVarGuard,
    _api_key: EnvVarGuard,
    _jwt_secret: EnvVarGuard,
    _jwt_audience: EnvVarGuard,
    _jwt_issuer: EnvVarGuard,
    _legacy_token: EnvVarGuard,
    _insecure_allow_no_auth: EnvVarGuard,
}

impl CommonL2Env {
    fn new() -> Self {
        Self {
            _max_connections: EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS", "0"),
            _max_connections_per_session: EnvVarGuard::set(
                "AERO_L2_MAX_CONNECTIONS_PER_SESSION",
                "0",
            ),
            _max_bytes: EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "0"),
            _max_fps: EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "0"),
            _ping_interval: EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0"),
            // Default to explicit unauthenticated mode so tests don't rely on implicit "no auth"
            // defaults (production config fails fast when no auth is configured).
            _auth_mode: EnvVarGuard::set("AERO_L2_AUTH_MODE", "none"),
            _api_key: EnvVarGuard::unset("AERO_L2_API_KEY"),
            _jwt_secret: EnvVarGuard::unset("AERO_L2_JWT_SECRET"),
            // Ensure developer shells don't accidentally harden or otherwise change proxy behavior
            // for these integration tests.
            _jwt_audience: EnvVarGuard::unset("AERO_L2_JWT_AUDIENCE"),
            _jwt_issuer: EnvVarGuard::unset("AERO_L2_JWT_ISSUER"),
            _session_secret: EnvVarGuard::unset("AERO_L2_SESSION_SECRET"),
            _session_secret_alias: EnvVarGuard::unset("SESSION_SECRET"),
            _gateway_session_secret: EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET"),
            _legacy_token: EnvVarGuard::unset("AERO_L2_TOKEN"),
            _insecure_allow_no_auth: EnvVarGuard::unset("AERO_L2_INSECURE_ALLOW_NO_AUTH"),
        }
    }
}

fn base_ws_request(addr: SocketAddr) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req
}

fn assert_http_status(err: WsError, expected: StatusCode) {
    match err {
        WsError::Http(resp) => assert_eq!(resp.status(), expected),
        other => panic!("expected http error {expected}, got {other:?}"),
    }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_with_retry(
    mut make_req: impl FnMut() -> tokio_tungstenite::tungstenite::http::Request<()>,
) -> WsStream {
    // Establishing a WebSocket connection can race with server-side permit release after the client
    // initiates a close. Avoid fixed sleeps here to keep CI deterministic under load.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match tokio_tungstenite::connect_async(make_req()).await {
            Ok((ws, _resp)) => return ws,
            Err(WsError::Http(resp)) if resp.status() == StatusCode::TOO_MANY_REQUESTS => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("timed out waiting for server-side session permit to be released");
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(err) => panic!("unexpected websocket connection error: {err:?}"),
        }
    }
}

fn parse_metric(body: &str, name: &str) -> Option<u64> {
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(' ')?;
        if k == name {
            return v.parse().ok();
        }
    }
    None
}

fn mint_session_token(secret: &str, sid: &str, exp_secs: u64) -> String {
    auth::mint_session_token(
        &auth::SessionClaims {
            sid: sid.to_string(),
            exp: exp_secs,
        },
        secret.as_bytes(),
    )
    .expect("mint session token")
}

fn tamper_session_token(token: &str) -> String {
    let (payload, sig) = token.split_once('.').unwrap();
    let mut sig_bytes = sig.as_bytes().to_vec();
    sig_bytes[0] = if sig_bytes[0] == b'A' { b'B' } else { b'A' };
    let sig = String::from_utf8(sig_bytes).unwrap();
    format!("{payload}.{sig}")
}

fn mint_jwt_token(
    secret: &str,
    sid: &str,
    exp_secs: u64,
    aud: Option<&str>,
    iss: Option<&str>,
) -> String {
    auth::mint_relay_jwt_hs256(
        &auth::RelayJwtClaims {
            sid: sid.to_string(),
            exp: i64::try_from(exp_secs).expect("exp i64"),
            iat: i64::try_from(exp_secs.saturating_sub(60)).expect("iat i64"),
            origin: None,
            aud: aud.map(|v| v.to_string()),
            iss: iss.map(|v| v.to_string()),
        },
        secret.as_bytes(),
    )
    .expect("mint relay jwt")
}

fn now_unix_seconds() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => dur.as_secs(),
        Err(err) => err.duration().as_secs(),
    }
}

fn make_relay_jwt(secret: &str, sid: &str, exp: u64, origin: Option<&str>) -> String {
    let now = now_unix_seconds();
    let claims = auth::RelayJwtClaims {
        sid: sid.to_string(),
        iat: i64::try_from(now.saturating_sub(1)).expect("iat i64"),
        exp: i64::try_from(exp).expect("exp i64"),
        origin: origin.map(|s| s.to_string()),
        aud: None,
        iss: None,
    };
    auth::mint_relay_jwt_hs256(&claims, secret.as_bytes()).expect("mint relay jwt")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subprotocol_required_rejects_missing_protocol() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    for path in ["/l2", "/l2/", "/eth", "/eth/"] {
        let ws_url = format!("ws://{addr}{path}");
        let req = ws_url.into_client_request().unwrap();
        let err = tokio_tungstenite::connect_async(req)
            .await
            .expect_err("expected missing subprotocol to be rejected");
        assert_http_status(err, StatusCode::BAD_REQUEST);
    }

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn origin_required_by_default_rejects_missing_origin() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected origin enforcement to reject missing Origin");
    assert_http_status(err, StatusCode::FORBIDDEN);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_allowed_origins_still_requires_origin_header() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing Origin to be rejected even with wildcard allowlist");
    assert_http_status(err, StatusCode::FORBIDDEN);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_origin_headers_are_rejected() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .append("origin", HeaderValue::from_static("https://allowed.test"));
    req.headers_mut()
        .append("origin", HeaderValue::from_static("https://blocked.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected multiple Origin headers to be rejected");
    assert_http_status(err, StatusCode::FORBIDDEN);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn origin_allowlist_and_open_mode() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");

    // Allowlist enforcement.
    {
        let _open = EnvVarGuard::unset("AERO_L2_OPEN");
        let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "HTTPS://ALLOWED.TEST:443/");

        let cfg = ProxyConfig::from_env().unwrap();
        let proxy = start_server(cfg).await.unwrap();
        let addr = proxy.local_addr();

        // Allowed origin succeeds.
        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://allowed.test"));
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let _ = ws.send(Message::Close(None)).await;

        // Disallowed origin rejected.
        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://blocked.test"));
        let err = tokio_tungstenite::connect_async(req)
            .await
            .expect_err("expected allowlist to reject disallowed Origin");
        assert_http_status(err, StatusCode::FORBIDDEN);

        proxy.shutdown().await;
    }

    // Open mode accepts missing Origin.
    {
        let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
        let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");

        let cfg = ProxyConfig::from_env().unwrap();
        let proxy = start_server(cfg).await.unwrap();
        let addr = proxy.local_addr();

        let req = base_ws_request(addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let _ = ws.send(Message::Close(None)).await;

        proxy.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_still_rejects_invalid_origin_values() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://evil.test/path"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected invalid Origin header to be rejected");
    assert_http_status(err, StatusCode::FORBIDDEN);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_allowlist_entry_rejected() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    // Paths are rejected; allowed origins must be bare origins.
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "https://example.com/path");
    ProxyConfig::from_env().expect_err("expected invalid origin allowlist entry to be rejected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_origins_fallback_to_shared_env_var() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::set("ALLOWED_ORIGINS", "https://allowed.test");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://allowed.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_aero_allowed_origins_falls_back_to_shared_env_var() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "");
    let _fallback_allowed = EnvVarGuard::set("ALLOWED_ORIGINS", "https://allowed.test");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://allowed.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comma_only_aero_allowed_origins_falls_back_to_shared_env_var() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    // Some deployment templates may produce comma-only placeholders; treat these as unset.
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", ",,");
    let _fallback_allowed = EnvVarGuard::set("ALLOWED_ORIGINS", "https://allowed.test");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://allowed.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_origins_extra_appends_without_replacing_base() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::set("ALLOWED_ORIGINS", "https://base.test");
    let _allowed_extra = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS_EXTRA", ",https://extra.test");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Origin allowed via base list.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://base.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Origin allowed via extra appended list.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://extra.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_required_query_and_subprotocol() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");
    let _auth_mode = EnvVarGuard::unset("AERO_L2_AUTH_MODE");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _jwt_secret = EnvVarGuard::unset("AERO_L2_JWT_SECRET");
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Query param token.
    let ws_url = format!("ws://{addr}/l2?token=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Query param apiKey (alternate key name used by some clients).
    let ws_url = format!("ws://{addr}/l2?apiKey=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Subprotocol token.
    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!(
            "{TUNNEL_SUBPROTOCOL}, {}sekrit",
            aero_l2_protocol::L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX
        ))
        .unwrap(),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Wrong token rejected.
    let ws_url = format!("ws://{addr}/l2?token=wrong");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected wrong token to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_auth_mode_accepts_query_and_subprotocol_tokens() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "api_key");
    let _api_key = EnvVarGuard::set("AERO_L2_API_KEY", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing token should be rejected (Origin is not required in open mode).
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected api_key auth to reject missing token");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Query param token.
    let ws_url = format!("ws://{addr}/l2?token=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Query param apiKey.
    let ws_url = format!("ws://{addr}/l2?apiKey=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Subprotocol token.
    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!(
            "{TUNNEL_SUBPROTOCOL}, {}sekrit",
            aero_l2_protocol::L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX
        ))
        .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Wrong token rejected.
    let ws_url = format!("ws://{addr}/l2?token=wrong");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected wrong api key to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_auth_mode_falls_back_to_legacy_token_env() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "api_key");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // `AERO_L2_TOKEN` should be accepted as a fallback value for `AERO_L2_API_KEY`.
    let ws_url = format!("ws://{addr}/l2?apiKey=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jwt_auth_accepts_query_and_subprotocol_tokens() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    // Ensure legacy token settings never "accidentally" enable api_key mode.
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "jwt");
    let _jwt_secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "jwt-sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing token should be rejected (Origin is not required in open mode).
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected jwt auth to reject missing token");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // A non-JWT `token=...` value should be rejected, even if it matches AERO_L2_TOKEN.
    let ws_url = format!("ws://{addr}/l2?token=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected jwt auth to reject non-jwt token");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let exp = now_unix_seconds().saturating_add(60);
    let token = make_relay_jwt("jwt-sekrit", "sid-jwt", exp, None);

    // Query param token.
    let ws_url = format!("ws://{addr}/l2?token={token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Query param apiKey (alternate key name).
    let ws_url = format!("ws://{addr}/l2?apiKey={token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Subprotocol token.
    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!(
            "{TUNNEL_SUBPROTOCOL}, {}{token}",
            aero_l2_protocol::L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX
        ))
        .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Wrong token rejected.
    let ws_url = format!("ws://{addr}/l2?token=wrong");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected invalid jwt to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jwt_origin_claim_is_enforced() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "jwt");
    let _jwt_secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let exp = now_unix_seconds().saturating_add(60);

    // Origin claim matches Origin header => success.
    let ok_token = make_relay_jwt("sekrit", "sid-jwt", exp, Some("https://any.test"));
    let ws_url = format!("ws://{addr}/l2?token={ok_token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Origin claim mismatch => rejected with 401.
    let bad_token = make_relay_jwt("sekrit", "sid-jwt", exp, Some("https://other.test"));
    let ws_url = format!("ws://{addr}/l2?token={bad_token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected jwt origin claim mismatch to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cookie_or_jwt_accepts_either_auth_mechanism() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie_or_jwt");
    let _session_secret = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "cookie-sekrit");
    let _jwt_secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "jwt-sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Cookie auth succeeds.
    let exp = now_unix_seconds().saturating_add(60);
    let cookie_token = mint_session_token("cookie-sekrit", "sid-cookie", exp);
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={cookie_token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // JWT auth also succeeds.
    let jwt_token = make_relay_jwt("jwt-sekrit", "sid-jwt", exp, None);
    let ws_url = format!("ws://{addr}/l2?token={jwt_token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Invalid cookie but valid JWT still succeeds.
    let expired = exp.saturating_sub(120);
    let bad_cookie = mint_session_token("cookie-sekrit", "sid-cookie", expired);
    let ws_url = format!("ws://{addr}/l2?token={jwt_token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={bad_cookie}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cookie_or_jwt_accepts_either_credential() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie_or_jwt");
    let _session_secret = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "sekrit");
    let _jwt_secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "jwt-sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing both credentials should be rejected.
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected cookie_or_jwt auth to reject missing credentials");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now.saturating_add(60);

    // Cookie credential works.
    let token = mint_session_token("sekrit", "sid", exp);
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // JWT credential works.
    let jwt = auth::mint_relay_jwt_hs256(
        &auth::RelayJwtClaims {
            sid: "sid".to_string(),
            iat: i64::try_from(now).expect("iat i64"),
            exp: i64::try_from(exp).expect("exp i64"),
            origin: None,
            aud: None,
            iss: None,
        },
        b"jwt-sekrit",
    )
    .expect("mint relay jwt");
    let ws_url = format!("ws://{addr}/l2?token={jwt}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cookie_or_api_key_accepts_either_credential() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie_or_api_key");
    let _secret = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "sekrit");
    let _api_key = EnvVarGuard::set("AERO_L2_API_KEY", "apisecrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing both credentials should be rejected.
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected cookie_or_api_key auth to reject missing credentials");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);

    // Cookie credential works.
    let token = mint_session_token("sekrit", "sid", exp);
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // API key credential works.
    let ws_url = format!("ws://{addr}/l2?apiKey=apisecrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Invalid cookie but valid API key should still succeed.
    let bad_cookie = tamper_session_token(&mint_session_token("sekrit", "sid", exp));
    let ws_url = format!("ws://{addr}/l2?apiKey=apisecrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={bad_cookie}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Invalid api key is rejected (even if a cookie is present but invalid).
    let ws_url = format!("ws://{addr}/l2?apiKey=wrong");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={bad_cookie}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected invalid api key to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_allowlist_rejects_mismatch() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::set("AERO_L2_ALLOWED_HOSTS", "allowed.test");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Matching host succeeds (with default-port normalization).
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("host", HeaderValue::from_static("allowed.test:80"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Mismatched host is rejected.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("host", HeaderValue::from_static("blocked.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected host allowlist to reject mismatched Host");
    assert_http_status(err, StatusCode::FORBIDDEN);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let rejected = parse_metric(&body, "l2_upgrade_reject_host_not_allowed_total").unwrap();
    assert!(
        rejected >= 1,
        "expected host-not-allowed reject counter >= 1, got {rejected}"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trust_proxy_host_allows_forwarded_host_to_satisfy_allowlist() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::set("AERO_L2_ALLOWED_HOSTS", "allowed.test");

    // Without trusting proxy host headers, X-Forwarded-Host should not affect the allowlist.
    {
        let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");

        let cfg = ProxyConfig::from_env().unwrap();
        let proxy = start_server(cfg).await.unwrap();
        let addr = proxy.local_addr();

        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("host", HeaderValue::from_static("blocked.test"));
        req.headers_mut()
            .insert("x-forwarded-host", HeaderValue::from_static("allowed.test"));
        let err = tokio_tungstenite::connect_async(req)
            .await
            .expect_err("expected untrusted X-Forwarded-Host to be ignored");
        assert_http_status(err, StatusCode::FORBIDDEN);

        proxy.shutdown().await;
    }

    // When trusting proxy host headers, prefer X-Forwarded-Host + X-Forwarded-Proto over Host.
    {
        let _trust_proxy_host = EnvVarGuard::set("AERO_L2_TRUST_PROXY_HOST", "1");

        let cfg = ProxyConfig::from_env().unwrap();
        let proxy = start_server(cfg).await.unwrap();
        let addr = proxy.local_addr();

        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("host", HeaderValue::from_static("blocked.test"));
        req.headers_mut().insert(
            "x-forwarded-host",
            HeaderValue::from_static("allowed.test:443"),
        );
        req.headers_mut()
            .insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let _ = ws.send(Message::Close(None)).await;

        proxy.shutdown().await;
    }

    // When trusting proxy host headers, accept the RFC7239 Forwarded header as well.
    {
        let _trust_proxy_host = EnvVarGuard::set("AERO_L2_TRUST_PROXY_HOST", "1");

        let cfg = ProxyConfig::from_env().unwrap();
        let proxy = start_server(cfg).await.unwrap();
        let addr = proxy.local_addr();

        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("host", HeaderValue::from_static("blocked.test"));
        req.headers_mut().insert(
            "forwarded",
            HeaderValue::from_static("for=203.0.113.1;host=\"allowed.test:443\";proto=\"https\""),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let _ = ws.send(Message::Close(None)).await;

        proxy.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_allowlist_rejects_invalid_host_and_increments_metric() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::set("AERO_L2_ALLOWED_HOSTS", "allowed.test");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("host", HeaderValue::from_static("allowed.test:abc"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected invalid host header to be rejected");
    assert_http_status(err, StatusCode::FORBIDDEN);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let rejected = parse_metric(&body, "l2_upgrade_reject_host_invalid_total").unwrap();
    assert!(
        rejected >= 1,
        "expected host-invalid reject counter >= 1, got {rejected}"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cookie_auth_requires_valid_session_cookie() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie");
    let _secret = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing cookie should be rejected (Origin is not required in open mode).
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected cookie auth to reject missing session cookie");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let auth_failures = parse_metric(&body, "l2_auth_failures_total").unwrap();
    assert!(
        auth_failures >= 1,
        "expected auth failure counter >= 1, got {auth_failures}"
    );

    // Valid cookie should succeed.
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);
    let token = mint_session_token("sekrit", "sid", exp);
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Cookie parsing matches the gateway implementation: tolerate unrelated/malformed cookie
    // segments and multiple Cookie headers.
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("foo; aero_session={token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .append("cookie", HeaderValue::from_static("foo=bar"));
    req.headers_mut().append(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // First cookie wins (matches gateway): a later valid cookie must not bypass an earlier invalid
    // aero_session value.
    let bad_token = tamper_session_token(&mint_session_token("sekrit", "sid", exp));
    let mut req = base_ws_request(addr);
    req.headers_mut().append(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={bad_token}")).unwrap(),
    );
    req.headers_mut().append(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected first invalid cookie to poison the request");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Empty cookie values are treated as missing but still \"win\" over later values (matches gateway
    // `if (!value) return null` behavior).
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .append("cookie", HeaderValue::from_static("aero_session="));
    req.headers_mut().append(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected empty cookie to poison the request");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Expired cookies should be rejected.
    let expired = exp.saturating_sub(120);
    let token = mint_session_token("sekrit", "sid", expired);
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected expired session cookie to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Tampered cookies should be rejected.
    let token = tamper_session_token(&mint_session_token("sekrit", "sid", exp));
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected tampered session cookie to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jwt_auth_accepts_bearer_and_query_and_validates_audience_and_issuer() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "jwt");
    let _secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "jwtsekrit");
    let _aud = EnvVarGuard::set("AERO_L2_JWT_AUDIENCE", "l2");
    let _iss = EnvVarGuard::set("AERO_L2_JWT_ISSUER", "gateway");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);
    let token = mint_jwt_token("jwtsekrit", "sid-jwt", exp, Some("l2"), Some("gateway"));

    // Missing token is rejected.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected jwt auth to reject missing token");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Bearer token succeeds.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Query token succeeds.
    let ws_url = format!("ws://{addr}/l2?token={token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Wrong audience rejected.
    let bad_aud = mint_jwt_token("jwtsekrit", "sid-jwt", exp, Some("wrong"), Some("gateway"));
    let ws_url = format!("ws://{addr}/l2?token={bad_aud}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected wrong-audience jwt to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Wrong issuer rejected.
    let bad_iss = mint_jwt_token("jwtsekrit", "sid-jwt", exp, Some("l2"), Some("wrong"));
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {bad_iss}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected wrong-issuer jwt to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_connections_per_session_enforced_for_jwt() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "jwt");
    let _secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "jwtsekrit");
    let _max = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS_PER_SESSION", "1");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);
    let token = mint_jwt_token("jwtsekrit", "sid-jwt", exp, None, None);

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    let (mut ws1, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected per-session tunnel limit enforcement for jwt auth");
    assert_http_status(err, StatusCode::TOO_MANY_REQUESTS);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let denied = parse_metric(&body, "l2_session_connection_denied_total").unwrap();
    assert!(
        denied >= 1,
        "expected session connection denied counter >= 1, got {denied}"
    );

    let _ = ws1.send(Message::Close(None)).await;

    let mut ws2 = connect_with_retry(|| {
        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://any.test"));
        req.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        req
    })
    .await;
    let _ = ws2.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cookie_auth_falls_back_to_session_secret() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie");
    // Docker Compose stacks commonly pass through `AERO_L2_SESSION_SECRET=` even when relying on
    // `SESSION_SECRET` as the canonical knob; ensure we treat empty values as unset.
    let _secret = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "");
    let _fallback_secret = EnvVarGuard::set("SESSION_SECRET", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);
    let token = mint_session_token("sekrit", "sid", exp);

    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_or_token_accepts_token_without_cookie() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session_or_token");
    let _secret = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "cookie-sekrit");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "tok-sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Token works without a session cookie.
    let ws_url = format!("ws://{addr}/l2?token=tok-sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_and_token_requires_both_mechanisms() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session_and_token");
    let _secret = EnvVarGuard::set("AERO_GATEWAY_SESSION_SECRET", "cookie-sekrit");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "tok-sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);
    let cookie_token = mint_session_token("cookie-sekrit", "sid-test", exp);
    let cookie = format!("aero_session={cookie_token}");

    // Cookie without token is rejected.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("cookie", HeaderValue::from_str(&cookie).unwrap());
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing token to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Token without cookie is rejected.
    let ws_url = format!("ws://{addr}/l2?token=tok-sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing cookie to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Cookie + token succeeds.
    let ws_url = format!("ws://{addr}/l2?token=tok-sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("cookie", HeaderValue::from_str(&cookie).unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_mode_disables_origin_but_not_token_auth() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");
    let _auth_mode = EnvVarGuard::unset("AERO_L2_AUTH_MODE");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _jwt_secret = EnvVarGuard::unset("AERO_L2_JWT_SECRET");
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing token should be rejected even though Origin is not required in open mode.
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected token enforcement even when open mode is enabled");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Providing a valid token should succeed without an Origin header.
    let ws_url = format!("ws://{addr}/l2?token=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_rejection_metrics_increment_for_missing_and_invalid_token() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "token");
    let _api_key = EnvVarGuard::set("AERO_L2_API_KEY", "sekrit");
    let _legacy_token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing token => auth_missing.
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing token to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Invalid token => auth_invalid.
    let ws_url = format!("ws://{addr}/l2?token=wrong");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected invalid token to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let missing = parse_metric(&body, "l2_upgrade_reject_auth_missing_total").unwrap();
    assert!(
        missing >= 1,
        "expected auth-missing reject counter >= 1, got {missing}"
    );
    let invalid = parse_metric(&body, "l2_upgrade_reject_auth_invalid_total").unwrap();
    assert!(
        invalid >= 1,
        "expected auth-invalid reject counter >= 1, got {invalid}"
    );
    let failures = parse_metric(&body, "l2_auth_failures_total").unwrap();
    assert!(
        failures >= 2,
        "expected auth failures counter >= 2, got {failures}"
    );

    let missing_label = parse_metric(
        &body,
        r#"l2_auth_reject_total{reason="missing_credentials"}"#,
    )
    .unwrap();
    assert!(
        missing_label >= 1,
        "expected missing-credentials label counter >= 1, got {missing_label}"
    );
    // Kept as `invalid_api_key` for historical compatibility; it corresponds to token auth too.
    let invalid_label =
        parse_metric(&body, r#"l2_auth_reject_total{reason="invalid_api_key"}"#).unwrap();
    assert!(
        invalid_label >= 1,
        "expected invalid-api-key label counter >= 1, got {invalid_label}"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_rejection_metrics_increment_for_invalid_session_cookie() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "session");
    let _secret = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let start = parse_metric(
        &baseline,
        r#"l2_auth_reject_total{reason="invalid_cookie"}"#,
    )
    .unwrap_or(0);

    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_static("aero_session=not-a-session-token"),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected invalid session cookie to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let val = parse_metric(&body, r#"l2_auth_reject_total{reason="invalid_cookie"}"#).unwrap_or(0);
    assert!(
        val >= start.saturating_add(1),
        "expected invalid-cookie label counter to increment (before={start}, after={val})"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_rejection_metrics_increment_for_invalid_jwt() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "jwt");
    let _secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let start =
        parse_metric(&baseline, r#"l2_auth_reject_total{reason="invalid_jwt"}"#).unwrap_or(0);

    let ws_url = format!("ws://{addr}/l2?token=not-a-jwt");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected invalid jwt to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let val = parse_metric(&body, r#"l2_auth_reject_total{reason="invalid_jwt"}"#).unwrap_or(0);
    assert!(
        val >= start.saturating_add(1),
        "expected invalid-jwt label counter to increment (before={start}, after={val})"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_rejection_metrics_increment_for_jwt_origin_mismatch() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "jwt");
    let _secret = EnvVarGuard::set("AERO_L2_JWT_SECRET", "sekrit");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let start = parse_metric(
        &baseline,
        r#"l2_auth_reject_total{reason="jwt_origin_mismatch"}"#,
    )
    .unwrap_or(0);

    let exp = now_unix_seconds().saturating_add(60);
    let token = make_relay_jwt("sekrit", "sid-jwt", exp, Some("https://expected.test"));
    let ws_url = format!("ws://{addr}/l2?token={token}");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://actual.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected jwt origin mismatch to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let val = parse_metric(
        &body,
        r#"l2_auth_reject_total{reason="jwt_origin_mismatch"}"#,
    )
    .unwrap_or(0);
    assert!(
        val >= start.saturating_add(1),
        "expected jwt-origin-mismatch label counter to increment (before={start}, after={val})"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_errors_take_precedence_over_origin_errors() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");
    let _auth_mode = EnvVarGuard::unset("AERO_L2_AUTH_MODE");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _jwt_secret = EnvVarGuard::unset("AERO_L2_JWT_SECRET");
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // Missing token should return 401 even if Origin is missing.
    let req = base_ws_request(addr);
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing token to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    // Valid token but missing Origin should return 403.
    let ws_url = format!("ws://{addr}/l2?token=sekrit");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing origin to be rejected");
    assert_http_status(err, StatusCode::FORBIDDEN);

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_connections_enforced() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _max_conn = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS", "1");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws1, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected max-connections enforcement");
    assert_http_status(err, StatusCode::TOO_MANY_REQUESTS);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let rejected = parse_metric(&body, "l2_upgrade_reject_max_connections_total").unwrap();
    assert!(
        rejected >= 1,
        "expected max-connections reject counter >= 1, got {rejected}"
    );

    let _ = ws1.send(Message::Close(None)).await;

    let mut ws2 = connect_with_retry(|| {
        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://any.test"));
        req
    })
    .await;
    let _ = ws2.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_connections_per_session_enforced() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "cookie");
    let _secret = EnvVarGuard::set("AERO_L2_SESSION_SECRET", "sekrit");
    let _max_per_session = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS_PER_SESSION", "1");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);
    let token = mint_session_token("sekrit", "sid-test", exp);
    let cookie = format!("aero_session={token}");

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("cookie", HeaderValue::from_str(&cookie).unwrap());
    let (mut ws1, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("cookie", HeaderValue::from_str(&cookie).unwrap());
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected per-session tunnel limit enforcement");
    assert_http_status(err, StatusCode::TOO_MANY_REQUESTS);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let rejected =
        parse_metric(&body, "l2_upgrade_reject_max_connections_per_session_total").unwrap();
    assert!(
        rejected >= 1,
        "expected per-session reject counter >= 1, got {rejected}"
    );
    let rejected = parse_metric(&body, "l2_upgrade_reject_max_tunnels_per_session_total").unwrap();
    assert!(
        rejected >= 1,
        "expected legacy per-session reject counter >= 1, got {rejected}"
    );
    let denied = parse_metric(&body, "l2_session_connection_denied_total").unwrap();
    assert!(
        denied >= 1,
        "expected session connection denied counter >= 1, got {denied}"
    );

    let _ = ws1.send(Message::Close(None)).await;

    let mut ws2 = connect_with_retry(|| {
        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://any.test"));
        req.headers_mut()
            .insert("cookie", HeaderValue::from_str(&cookie).unwrap());
        req
    })
    .await;
    let _ = ws2.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_connections_per_ip_enforced_with_x_forwarded_for_when_trusting_proxy() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _trust_proxy = EnvVarGuard::set("AERO_L2_TRUST_PROXY", "1");
    let _max_per_ip = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS_PER_IP", "1");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));
    let (mut ws1, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // A different forwarded IP should be allowed concurrently.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.2"));
    let (mut ws2, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // A second connection from the same forwarded IP is rejected.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected per-IP max connections enforcement");
    assert_http_status(err, StatusCode::TOO_MANY_REQUESTS);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let exceeded = parse_metric(&body, "l2_upgrade_ip_limit_exceeded_total").unwrap();
    assert!(
        exceeded >= 1,
        "expected ip-limit exceeded counter >= 1, got {exceeded}"
    );

    let _ = ws1.send(Message::Close(None)).await;

    let mut ws3 = connect_with_retry(|| {
        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://any.test"));
        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));
        req
    })
    .await;
    let _ = ws3.send(Message::Close(None)).await;

    let _ = ws2.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_connections_per_ip_enforced_with_forwarded_header_when_trusting_proxy() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _trust_proxy = EnvVarGuard::set("AERO_L2_TRUST_PROXY", "1");
    let _max_per_ip = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS_PER_IP", "1");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("forwarded", HeaderValue::from_static("for=203.0.113.1"));
    let (mut ws1, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // A different forwarded IP should be allowed concurrently.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("forwarded", HeaderValue::from_static("for=203.0.113.2"));
    let (mut ws2, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // A second connection from the same forwarded IP is rejected.
    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    req.headers_mut()
        .insert("forwarded", HeaderValue::from_static("for=203.0.113.1"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected per-IP max connections enforcement");
    assert_http_status(err, StatusCode::TOO_MANY_REQUESTS);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let exceeded = parse_metric(&body, "l2_upgrade_ip_limit_exceeded_total").unwrap();
    assert!(
        exceeded >= 1,
        "expected ip-limit exceeded counter >= 1, got {exceeded}"
    );

    let _ = ws1.send(Message::Close(None)).await;

    let mut ws3 = connect_with_retry(|| {
        let mut req = base_ws_request(addr);
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://any.test"));
        req.headers_mut()
            .insert("forwarded", HeaderValue::from_static("for=203.0.113.1"));
        req
    })
    .await;
    let _ = ws3.send(Message::Close(None)).await;

    let _ = ws2.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn byte_quota_closes_connection() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _quota = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "100");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let payload = vec![0u8; 60];
    let wire = aero_l2_protocol::encode_pong(Some(&payload)).unwrap();

    for _ in 0..10 {
        if ws.send(Message::Binary(wire.clone().into())).await.is_err() {
            break;
        }
    }

    let (err_msg, close) = tokio::time::timeout(Duration::from_secs(2), async {
        let mut err_msg: Option<(u16, String)> = None;
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(buf))) => {
                    let Ok(decoded) = aero_l2_protocol::decode_message(buf.as_ref()) else {
                        continue;
                    };
                    if decoded.msg_type == aero_l2_protocol::L2_TUNNEL_TYPE_ERROR {
                        if let Some(parsed) = protocol::decode_error_payload(decoded.payload) {
                            err_msg = Some(parsed);
                        }
                    }
                }
                Some(Ok(Message::Close(frame))) => return (err_msg, frame),
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return (err_msg, None),
            }
        }
    })
    .await
    .unwrap();

    let frame = close.expect("expected close frame");
    assert_eq!(
        frame.code,
        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy
    );
    let (code, msg) = err_msg.expect("expected ERROR control message before close");
    assert_eq!(code, protocol::ERROR_CODE_QUOTA_BYTES);
    assert_eq!(msg, "byte quota exceeded");

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn byte_quota_counts_tx_bytes() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _quota = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "40");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // 20-byte payload => 24-byte ws message. With max_bytes=40, the inbound ping is allowed but
    // the outbound pong exceeds the rx+tx quota, proving tx bytes are counted.
    let payload = vec![0u8; 20];
    let ping = aero_l2_protocol::encode_ping(Some(&payload)).unwrap();
    ws.send(Message::Binary(ping.into())).await.unwrap();

    let (err_msg, close) = tokio::time::timeout(Duration::from_secs(2), async {
        let mut err_msg: Option<(u16, String)> = None;
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(buf))) => {
                    let Ok(decoded) = aero_l2_protocol::decode_message(buf.as_ref()) else {
                        continue;
                    };
                    if decoded.msg_type == aero_l2_protocol::L2_TUNNEL_TYPE_ERROR {
                        if let Some(parsed) = protocol::decode_error_payload(decoded.payload) {
                            err_msg = Some(parsed);
                        }
                    }
                }
                Some(Ok(Message::Close(frame))) => return (err_msg, frame),
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return (err_msg, None),
            }
        }
    })
    .await
    .unwrap();

    let frame = close.expect("expected close frame");
    assert_eq!(
        frame.code,
        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy
    );
    let (code, msg) = err_msg.expect("expected ERROR control message before close");
    assert_eq!(code, protocol::ERROR_CODE_QUOTA_BYTES);
    assert_eq!(msg, "byte quota exceeded");

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keepalive_ping_counts_toward_byte_quota() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    // A keepalive PING is 12 bytes (4-byte header + 8-byte ping id). Setting the quota below that
    // should cause the server-driven keepalive to immediately trigger a policy-violation close.
    let _quota = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "11");
    let _ping = EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "10");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = base_ws_request(addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let (err_msg, close) = tokio::time::timeout(Duration::from_secs(2), async {
        let mut err_msg: Option<(u16, String)> = None;
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(buf))) => {
                    let Ok(decoded) = aero_l2_protocol::decode_message(buf.as_ref()) else {
                        continue;
                    };
                    if decoded.msg_type == aero_l2_protocol::L2_TUNNEL_TYPE_ERROR {
                        if let Some(parsed) = protocol::decode_error_payload(decoded.payload) {
                            err_msg = Some(parsed);
                        }
                    }
                }
                Some(Ok(Message::Close(frame))) => return (err_msg, frame),
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return (err_msg, None),
            }
        }
    })
    .await
    .unwrap();

    let frame = close.expect("expected close frame");
    assert_eq!(
        frame.code,
        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy
    );
    let (code, msg) = err_msg.expect("expected ERROR control message before close");
    assert_eq!(code, protocol::ERROR_CODE_QUOTA_BYTES);
    assert_eq!(msg, "byte quota exceeded");

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fps_quota_closes_connection() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _fps = EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "2");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let wire = aero_l2_protocol::encode_pong(None).unwrap();
    for _ in 0..10 {
        if ws.send(Message::Binary(wire.clone().into())).await.is_err() {
            break;
        }
    }

    let (err_msg, close) = tokio::time::timeout(Duration::from_secs(2), async {
        let mut err_msg: Option<(u16, String)> = None;
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(buf))) => {
                    let Ok(decoded) = aero_l2_protocol::decode_message(buf.as_ref()) else {
                        continue;
                    };
                    if decoded.msg_type == aero_l2_protocol::L2_TUNNEL_TYPE_ERROR {
                        if let Some(parsed) = protocol::decode_error_payload(decoded.payload) {
                            err_msg = Some(parsed);
                        }
                    }
                }
                Some(Ok(Message::Close(frame))) => return (err_msg, frame),
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return (err_msg, None),
            }
        }
    })
    .await
    .unwrap();

    let frame = close.expect("expected close frame");
    assert_eq!(
        frame.code,
        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy
    );
    let (code, msg) = err_msg.expect("expected ERROR control message before close");
    assert_eq!(code, protocol::ERROR_CODE_QUOTA_FPS);
    assert_eq!(msg, "frame rate quota exceeded");

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_ws_messages_are_rejected_at_websocket_layer() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    // Force tiny protocol limits so we can trip the WebSocket size caps with a small message.
    let _max_frame_payload = EnvVarGuard::set("AERO_L2_MAX_FRAME_PAYLOAD", "1");
    let _max_control_payload = EnvVarGuard::set("AERO_L2_MAX_CONTROL_PAYLOAD", "1");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let payload = vec![0u8; 1024];
    let mut wire = Vec::with_capacity(aero_l2_protocol::L2_TUNNEL_HEADER_LEN + payload.len());
    wire.push(aero_l2_protocol::L2_TUNNEL_MAGIC);
    wire.push(aero_l2_protocol::L2_TUNNEL_VERSION);
    wire.push(aero_l2_protocol::L2_TUNNEL_TYPE_FRAME);
    wire.push(0);
    wire.extend_from_slice(&payload);

    ws.send(Message::Binary(wire.into())).await.unwrap();

    let close = tokio::time::timeout(Duration::from_secs(2), async {
        match ws.next().await {
            Some(Ok(Message::Close(frame))) => frame,
            Some(Ok(msg)) => panic!("expected connection close, got {msg:?}"),
            Some(Err(_)) => None,
            None => None,
        }
    })
    .await
    .unwrap();

    if let Some(frame) = close {
        assert_eq!(
            frame.code,
            tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Size
        );
    }

    // After an oversized message, the server should have closed the connection so further sends
    // must fail.
    assert!(ws.send(Message::Binary(vec![0u8; 1].into())).await.is_err());

    proxy.shutdown().await;
}
