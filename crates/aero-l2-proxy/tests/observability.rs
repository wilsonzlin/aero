use std::{net::SocketAddr, path::PathBuf, time::Duration};

use aero_l2_proxy::{start_server, EgressPolicy, ProxyConfig, TUNNEL_SUBPROTOCOL};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

struct TestServer {
    addr: SocketAddr,
    handle: aero_l2_proxy::ServerHandle,
}

impl TestServer {
    async fn start(capture_dir: Option<PathBuf>) -> Self {
        let cfg = ProxyConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            l2_max_frame_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
            l2_max_control_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
            tcp_connect_timeout: Duration::from_millis(200),
            tcp_send_buffer: 8,
            ws_send_buffer: 8,
            dns_default_ttl_secs: 60,
            dns_max_ttl_secs: 300,
            capture_dir,
            policy: EgressPolicy::from_env().unwrap(),
            test_overrides: Default::default(),
        };

        let handle = start_server(cfg).await.unwrap();
        let addr = handle.local_addr();
        Self { addr, handle }
    }

    fn http_url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn ws_url(&self) -> String {
        format!("ws://{}/l2", self.addr)
    }

    async fn shutdown(self) {
        self.handle.shutdown().await;
    }
}

#[tokio::test]
async fn metrics_increment_after_frames() {
    let server = TestServer::start(None).await;

    let mut req = server.ws_url().into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, _ws_receiver) = ws.split();

    // Payload is treated as an Ethernet frame by the proxy. It doesn't need to be valid for the
    // rx counters/capture path to be exercised.
    let frame = vec![0u8; 60];
    let wire = aero_l2_protocol::encode_frame(&frame).unwrap();
    ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Binary(wire.into()))
        .await
        .unwrap();

    let _ = ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await;

    let body = reqwest::get(server.http_url("/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let rx = parse_metric(&body, "l2_frames_rx_total").unwrap();
    assert!(rx >= 1, "expected rx counter >= 1, got {rx}");

    server.shutdown().await;
}

#[tokio::test]
async fn capture_creates_non_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(Some(dir.path().to_path_buf())).await;

    let mut req = server.ws_url().into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, _ws_receiver) = ws.split();

    let frame = vec![0u8; 60];
    let wire = aero_l2_protocol::encode_frame(&frame).unwrap();
    ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Binary(wire.into()))
        .await
        .unwrap();
    let _ = ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some((path, len)) = find_capture_file(dir.path()) {
                if len <= 128 {
                    // File was created but capture header/packets may not have been flushed yet.
                    // Keep waiting until it's non-empty.
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy();
                assert!(
                    name.contains("session-") && name.ends_with(".pcapng"),
                    "unexpected capture filename: {name}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    server.shutdown().await;
}

fn find_capture_file(dir: &std::path::Path) -> Option<(std::path::PathBuf, u64)> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("pcapng") {
            continue;
        }
        let len = entry.metadata().ok()?.len();
        return Some((path, len));
    }
    None
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
