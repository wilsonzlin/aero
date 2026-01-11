use std::{net::SocketAddr, sync::Mutex, time::Duration};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::{HeaderValue, StatusCode},
    Error as WsError, Message,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

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

#[tokio::test]
async fn origin_required_by_default_rejects_missing_origin() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
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
async fn origin_allowlist_and_open_mode() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");

    // Allowlist enforcement.
    {
        let _open = EnvVarGuard::unset("AERO_L2_OPEN");
        let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "https://allowed.test");

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
async fn token_required_query_and_subprotocol() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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
async fn max_connections_enforced() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
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
    let _lock = ENV_LOCK.lock().unwrap();
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _quota = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "100");
    let _fps = EnvVarGuard::unset("AERO_L2_MAX_FRAMES_PER_SECOND");

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
    assert_eq!(frame.code, tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy);

    proxy.shutdown().await;
}

#[tokio::test]
async fn fps_quota_closes_connection() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::unset("AERO_L2_OPEN");
    let _allowed = EnvVarGuard::set("AERO_L2_ALLOWED_ORIGINS", "*");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _quota = EnvVarGuard::unset("AERO_L2_MAX_BYTES_PER_CONNECTION");
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
    assert_eq!(frame.code, tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy);

    proxy.shutdown().await;
}
