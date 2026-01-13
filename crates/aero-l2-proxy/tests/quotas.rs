#![cfg(not(target_arch = "wasm32"))]

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::HeaderValue, protocol::frame::coding::CloseCode, Message,
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

fn encode_l2_frame(payload: &[u8]) -> Vec<u8> {
    aero_l2_protocol::encode_frame(payload).unwrap()
}

async fn fetch_metrics(addr: SocketAddr) -> String {
    reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

async fn wait_for_error_and_close(
    ws_rx: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    expected_code: u16,
) {
    let mut saw_error = false;
    let mut saw_close = false;
    let deadline = Instant::now() + Duration::from_secs(5);

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = match tokio::time::timeout(remaining, ws_rx.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        };

        match msg {
            Message::Binary(buf) => {
                let Ok(decoded) = aero_l2_protocol::decode_message(&buf) else {
                    continue;
                };
                if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_ERROR {
                    continue;
                }
                let (code, message) =
                    aero_l2_proxy::protocol::decode_error_payload(decoded.payload)
                        .expect("expected structured error payload");
                assert_eq!(code, expected_code);
                assert!(
                    !message.is_empty(),
                    "expected non-empty structured error message"
                );
                saw_error = true;
            }
            Message::Close(frame) => {
                let frame = frame.expect("expected close frame");
                assert_eq!(frame.code, CloseCode::Policy, "expected close code 1008");
                saw_close = true;
                break;
            }
            _ => {}
        }
    }

    assert!(saw_error, "expected ERROR message before close");
    assert!(saw_close, "expected websocket close");
}

fn common_env() -> Vec<EnvVarGuard> {
    vec![
        EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0"),
        EnvVarGuard::set("AERO_L2_OPEN", "1"),
        EnvVarGuard::set("AERO_L2_AUTH_MODE", "none"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA"),
        EnvVarGuard::unset("ALLOWED_ORIGINS"),
        EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS"),
        EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST"),
        EnvVarGuard::unset("AERO_L2_TOKEN"),
        EnvVarGuard::unset("AERO_L2_API_KEY"),
        EnvVarGuard::unset("AERO_L2_JWT_SECRET"),
        EnvVarGuard::unset("AERO_L2_JWT_AUDIENCE"),
        EnvVarGuard::unset("AERO_L2_JWT_ISSUER"),
        EnvVarGuard::unset("AERO_L2_SESSION_SECRET"),
        EnvVarGuard::unset("SESSION_SECRET"),
        EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET"),
        EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS", "0"),
        EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS_PER_SESSION", "0"),
        EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0"),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bytes_quota_sends_structured_error_and_closes() {
    let _lock = ENV_LOCK.lock().await;
    let _common = common_env();
    let _max_bytes = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "150");
    let _max_fps = EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "0");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline_body = fetch_metrics(addr).await;
    let sessions_start = parse_metric(&baseline_body, "l2_sessions_total").unwrap_or(0);
    let frames_rx_start = parse_metric(&baseline_body, "l2_frames_rx_total").unwrap_or(0);
    let bytes_rx_start = parse_metric(&baseline_body, "l2_bytes_rx_total").unwrap_or(0);
    let quota_start =
        parse_metric(&baseline_body, "l2_sessions_closed_quota_bytes_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let frame = vec![0u8; 64];
    let frame_msg = encode_l2_frame(&frame);

    // 64 bytes payload + 4 byte L2 header == 68 bytes per message.
    // With AERO_L2_MAX_BYTES_PER_CONNECTION=150, the third inbound frame should exceed the quota.
    for _ in 0..3 {
        ws_tx
            .send(Message::Binary(frame_msg.clone().into()))
            .await
            .unwrap();
    }

    wait_for_error_and_close(&mut ws_rx, aero_l2_proxy::protocol::ERROR_CODE_QUOTA_BYTES).await;

    let body = fetch_metrics(addr).await;
    let sessions = parse_metric(&body, "l2_sessions_total").unwrap_or(0);
    assert!(
        sessions >= sessions_start.saturating_add(1),
        "expected l2_sessions_total to increment (before={sessions_start}, after={sessions})"
    );
    let frames_rx = parse_metric(&body, "l2_frames_rx_total").unwrap_or(0);
    assert!(
        frames_rx >= frames_rx_start.saturating_add(1),
        "expected l2_frames_rx_total to increment (before={frames_rx_start}, after={frames_rx})"
    );
    let bytes_rx = parse_metric(&body, "l2_bytes_rx_total").unwrap_or(0);
    assert!(
        bytes_rx >= bytes_rx_start.saturating_add(64),
        "expected l2_bytes_rx_total to reflect received frames (before={bytes_rx_start}, after={bytes_rx})"
    );
    let quota = parse_metric(&body, "l2_sessions_closed_quota_bytes_total").unwrap_or(0);
    assert!(
        quota >= quota_start.saturating_add(1),
        "expected quota-bytes counter to increment (before={quota_start}, after={quota})"
    );

    let _ = ws_tx.send(Message::Close(None)).await;
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fps_quota_sends_structured_error_and_closes() {
    let _lock = ENV_LOCK.lock().await;
    let _common = common_env();
    let _max_bytes = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "0");
    // Allow only 1 inbound message per second; the second message should trigger the quota.
    let _max_fps = EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "1");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline_body = fetch_metrics(addr).await;
    let sessions_start = parse_metric(&baseline_body, "l2_sessions_total").unwrap_or(0);
    let frames_rx_start = parse_metric(&baseline_body, "l2_frames_rx_total").unwrap_or(0);
    let bytes_rx_start = parse_metric(&baseline_body, "l2_bytes_rx_total").unwrap_or(0);
    let quota_start =
        parse_metric(&baseline_body, "l2_sessions_closed_quota_fps_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let frame = vec![0u8; 64];
    let frame_msg = encode_l2_frame(&frame);
    // First frame is processed, second triggers the FPS quota.
    for _ in 0..2 {
        ws_tx
            .send(Message::Binary(frame_msg.clone().into()))
            .await
            .unwrap();
    }

    wait_for_error_and_close(&mut ws_rx, aero_l2_proxy::protocol::ERROR_CODE_QUOTA_FPS).await;

    let body = fetch_metrics(addr).await;
    let sessions = parse_metric(&body, "l2_sessions_total").unwrap_or(0);
    assert!(
        sessions >= sessions_start.saturating_add(1),
        "expected l2_sessions_total to increment (before={sessions_start}, after={sessions})"
    );
    let frames_rx = parse_metric(&body, "l2_frames_rx_total").unwrap_or(0);
    assert!(
        frames_rx >= frames_rx_start.saturating_add(1),
        "expected l2_frames_rx_total to increment (before={frames_rx_start}, after={frames_rx})"
    );
    let bytes_rx = parse_metric(&body, "l2_bytes_rx_total").unwrap_or(0);
    assert!(
        bytes_rx >= bytes_rx_start.saturating_add(64),
        "expected l2_bytes_rx_total to reflect received frames (before={bytes_rx_start}, after={bytes_rx})"
    );
    let quota = parse_metric(&body, "l2_sessions_closed_quota_fps_total").unwrap_or(0);
    assert!(
        quota >= quota_start.saturating_add(1),
        "expected quota-fps counter to increment (before={quota_start}, after={quota})"
    );

    let _ = ws_tx.send(Message::Close(None)).await;
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_backpressure_sends_structured_error_and_closes() {
    let _lock = ENV_LOCK.lock().await;
    let _common = common_env();
    let _max_bytes = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "0");
    let _max_fps = EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "0");
    let _ws_send_buffer = EnvVarGuard::set("AERO_L2_WS_SEND_BUFFER", "1");

    // Allow larger control payloads so we can flood large PING/PONG messages and deterministically
    // hit outbound backpressure without needing the full network stack to emit large frames.
    let max_control_payload = 16 * 1024;
    let _max_control_payload = EnvVarGuard::set(
        "AERO_L2_MAX_CONTROL_PAYLOAD",
        &max_control_payload.to_string(),
    );

    let cfg = ProxyConfig::from_env().unwrap();
    assert_eq!(cfg.l2_max_control_payload, max_control_payload);
    assert_eq!(cfg.ws_send_buffer, 1);
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline_body = fetch_metrics(addr).await;
    let sessions_start = parse_metric(&baseline_body, "l2_sessions_total").unwrap_or(0);
    let frames_rx_start = parse_metric(&baseline_body, "l2_frames_rx_total").unwrap_or(0);
    let bytes_rx_start = parse_metric(&baseline_body, "l2_bytes_rx_total").unwrap_or(0);
    let quota_start =
        parse_metric(&baseline_body, "l2_sessions_closed_backpressure_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Ensure frames_rx/bytes_rx metrics increment at least once for this session.
    let frame = vec![0u8; 64];
    ws_tx
        .send(Message::Binary(encode_l2_frame(&frame).into()))
        .await
        .unwrap();

    let l2_limits = aero_l2_protocol::Limits {
        max_frame_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
        max_control_payload,
    };
    let ping_payload = vec![0u8; max_control_payload];
    let ping_wire = aero_l2_protocol::encode_with_limits(
        aero_l2_protocol::L2_TUNNEL_TYPE_PING,
        0,
        &ping_payload,
        &l2_limits,
    )
    .unwrap();

    // Flood the server with large PING messages while *not* reading from the WebSocket. The server
    // responds with PONG messages of the same size, eventually stalling the websocket writer and
    // triggering the outbound backpressure quota.
    let mut ws_tx_flood = ws_tx;
    let flood_task = tokio::spawn(async move {
        for _ in 0..10_000u32 {
            if ws_tx_flood
                .send(Message::Binary(ping_wire.clone().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Give the server time to fill the outbound buffer and hit the `send_ws_message` timeout.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    wait_for_error_and_close(&mut ws_rx, aero_l2_proxy::protocol::ERROR_CODE_BACKPRESSURE).await;

    let _ = tokio::time::timeout(Duration::from_secs(2), flood_task).await;

    let body = fetch_metrics(addr).await;
    let sessions = parse_metric(&body, "l2_sessions_total").unwrap_or(0);
    assert!(
        sessions >= sessions_start.saturating_add(1),
        "expected l2_sessions_total to increment (before={sessions_start}, after={sessions})"
    );
    let frames_rx = parse_metric(&body, "l2_frames_rx_total").unwrap_or(0);
    assert!(
        frames_rx >= frames_rx_start.saturating_add(1),
        "expected l2_frames_rx_total to increment (before={frames_rx_start}, after={frames_rx})"
    );
    let bytes_rx = parse_metric(&body, "l2_bytes_rx_total").unwrap_or(0);
    assert!(
        bytes_rx >= bytes_rx_start.saturating_add(64),
        "expected l2_bytes_rx_total to reflect received frames (before={bytes_rx_start}, after={bytes_rx})"
    );
    let quota = parse_metric(&body, "l2_sessions_closed_backpressure_total").unwrap_or(0);
    assert!(
        quota >= quota_start.saturating_add(1),
        "expected backpressure counter to increment (before={quota_start}, after={quota})"
    );

    proxy.shutdown().await;
}
