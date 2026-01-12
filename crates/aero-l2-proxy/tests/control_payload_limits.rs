#![cfg(not(target_arch = "wasm32"))]

use std::{net::SocketAddr, time::Duration};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

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

fn ws_request(addr: SocketAddr) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_control_payload_is_rejected() {
    let _lock = ENV_LOCK.lock().await;

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _jwt_secret = EnvVarGuard::unset("AERO_L2_JWT_SECRET");
    let _jwt_audience = EnvVarGuard::unset("AERO_L2_JWT_AUDIENCE");
    let _jwt_issuer = EnvVarGuard::unset("AERO_L2_JWT_ISSUER");
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");
    let _session_secret_alias = EnvVarGuard::unset("SESSION_SECRET");
    let _gateway_session_secret = EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET");
    let _legacy_token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _ping_interval = EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0");
    let _max_connections = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS", "0");
    let _max_connections_per_session = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS_PER_SESSION", "0");
    let _max_bytes = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "0");
    let _max_fps = EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "0");
    let _max_control_payload = EnvVarGuard::set("AERO_L2_MAX_CONTROL_PAYLOAD", "8");

    let cfg = ProxyConfig::from_env().unwrap();
    assert_eq!(cfg.l2_max_control_payload, 8);

    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let dropped_start = parse_metric(&baseline, "l2_frames_dropped_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Payload intentionally exceeds the configured `AERO_L2_MAX_CONTROL_PAYLOAD=8`.
    let oversized_payload = vec![0u8; 16];
    let ping = aero_l2_protocol::encode_ping(Some(&oversized_payload)).unwrap();
    ws_tx.send(Message::Binary(ping.into())).await.unwrap();

    let mut saw_error = false;
    let mut saw_close = false;
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Binary(buf)) => {
                    let Ok(decoded) = aero_l2_protocol::decode_message(&buf) else {
                        continue;
                    };
                    if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_ERROR {
                        continue;
                    }
                    if let Some((code, message)) =
                        aero_l2_proxy::protocol::decode_error_payload(decoded.payload)
                    {
                        assert_eq!(code, aero_l2_proxy::protocol::ERROR_CODE_PROTOCOL_ERROR);
                        assert!(
                            !message.is_empty(),
                            "expected ERROR message to be non-empty"
                        );
                    }
                    saw_error = true;
                }
                Ok(Message::Close(frame)) => {
                    let frame = frame.expect("expected close frame");
                    assert_eq!(
                        frame.code,
                        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy,
                        "expected close code 1008"
                    );
                    saw_close = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(
        saw_close,
        "expected websocket close for oversized control payload"
    );
    assert!(saw_error, "expected ERROR message before close");

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let dropped = parse_metric(&body, "l2_frames_dropped_total").unwrap_or(0);
    assert!(
        dropped >= dropped_start.saturating_add(1),
        "expected dropped counter to increment (before={dropped_start}, after={dropped})"
    );

    let _ = ws_tx.send(Message::Close(None)).await;
    proxy.shutdown().await;
}
