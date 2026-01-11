use std::{
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use ring::hmac;
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
    _max_bytes: EnvVarGuard,
    _max_fps: EnvVarGuard,
    _ping_interval: EnvVarGuard,
}

impl CommonL2Env {
    fn new() -> Self {
        Self {
            _max_connections: EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS", "0"),
            _max_bytes: EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "0"),
            _max_fps: EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "0"),
            _ping_interval: EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0"),
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

fn make_session_token(secret: &str, sid: &str, exp_secs: u64) -> String {
    let payload = serde_json::json!({
        "v": 1,
        "sid": sid,
        "exp": exp_secs,
    });
    let payload_bytes = serde_json::to_vec(&payload).unwrap();
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_bytes);
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let sig = hmac::sign(&key, payload_b64.as_bytes());
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.as_ref());
    format!("{payload_b64}.{sig_b64}")
}

#[tokio::test]
async fn subprotocol_required_rejects_missing_protocol() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let ws_url = format!("ws://{addr}/l2");
    let req = ws_url.into_client_request().unwrap();
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing subprotocol to be rejected");
    assert_http_status(err, StatusCode::BAD_REQUEST);

    proxy.shutdown().await;
}

#[tokio::test]
async fn origin_required_by_default_rejects_missing_origin() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
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

#[tokio::test]
async fn wildcard_allowed_origins_still_requires_origin_header() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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

#[tokio::test]
async fn origin_allowlist_and_open_mode() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
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

#[tokio::test]
async fn wildcard_still_rejects_invalid_origin_values() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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

#[tokio::test]
async fn token_required_query_and_subprotocol() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");

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

    // Subprotocol token.
    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static("aero-l2-tunnel-v1, aero-l2-token.sekrit"),
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

#[tokio::test]
async fn host_allowlist_rejects_mismatch() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::set("AERO_L2_ALLOWED_HOSTS", "allowed.test");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _auth_mode = EnvVarGuard::unset("AERO_L2_AUTH_MODE");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _jwt_secret = EnvVarGuard::unset("AERO_L2_JWT_SECRET");
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");

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

    proxy.shutdown().await;
}

#[tokio::test]
async fn cookie_auth_requires_valid_session_cookie() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
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

    // Valid cookie should succeed.
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(60);
    let token = make_session_token("sekrit", "sid", exp);
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Close(None)).await;

    // Expired cookies should be rejected.
    let expired = exp.saturating_sub(120);
    let token = make_session_token("sekrit", "sid", expired);
    let mut req = base_ws_request(addr);
    req.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("aero_session={token}")).unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected expired session cookie to be rejected");
    assert_http_status(err, StatusCode::UNAUTHORIZED);

    proxy.shutdown().await;
}

#[tokio::test]
async fn open_mode_disables_origin_but_not_token_auth() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");

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

#[tokio::test]
async fn token_errors_take_precedence_over_origin_errors() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _token = EnvVarGuard::set("AERO_L2_TOKEN", "sekrit");

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

#[tokio::test]
async fn max_connections_enforced() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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

    let _ = ws1.send(Message::Close(None)).await;

    // Wait for the server-side session to observe the close and release the permit.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut req = base_ws_request(addr);
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://any.test"));
    let (mut ws2, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws2.send(Message::Close(None)).await;

    proxy.shutdown().await;
}

#[tokio::test]
async fn byte_quota_closes_connection() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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

    let close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame,
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return None,
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

    proxy.shutdown().await;
}

#[tokio::test]
async fn byte_quota_counts_tx_bytes() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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

    let close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame,
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return None,
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

    proxy.shutdown().await;
}

#[tokio::test]
async fn keepalive_ping_counts_toward_byte_quota() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
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

    let close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame,
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return None,
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

    proxy.shutdown().await;
}

#[tokio::test]
async fn fps_quota_closes_connection() {
    let _lock = ENV_LOCK.lock().await;
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _common = CommonL2Env::new();
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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

    let close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame,
                Some(Ok(_)) => continue,
                Some(Err(err)) => panic!("ws recv error: {err}"),
                None => return None,
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

    proxy.shutdown().await;
}
