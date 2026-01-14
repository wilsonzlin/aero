#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

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
    fn set(key: &'static str, value: impl AsRef<str>) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value.as_ref());
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

fn reset_env(capture_dir: &str, capture_max_bytes: &str) -> Vec<EnvVarGuard> {
    // Ensure developer env vars don't accidentally harden the proxy in these env-based tests.
    vec![
        EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0"),
        EnvVarGuard::set("AERO_L2_OPEN", "1"),
        EnvVarGuard::set("AERO_L2_AUTH_MODE", "none"),
        // Disable Host allowlisting (if inherited from the developer environment).
        EnvVarGuard::set("AERO_L2_ALLOWED_HOSTS", ""),
        // Enable capture.
        EnvVarGuard::set("AERO_L2_CAPTURE_DIR", capture_dir),
        EnvVarGuard::set("AERO_L2_CAPTURE_MAX_BYTES", capture_max_bytes),
        // Exercise "flush on close" mode.
        EnvVarGuard::set("AERO_L2_CAPTURE_FLUSH_INTERVAL_MS", "0"),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_env_creates_file_on_close() {
    let _lock = ENV_LOCK.lock().await;

    let dir = tempfile::tempdir().unwrap();
    let _guards = reset_env(dir.path().to_str().unwrap(), "0");

    let cfg = ProxyConfig::from_env().unwrap();
    let server = start_server(cfg).await.unwrap();
    let addr = server.local_addr();

    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, _ws_receiver) = ws.split();

    let frame = vec![0u8; 60];
    let wire = aero_l2_protocol::encode_frame(&frame).unwrap();
    ws_sender.send(Message::Binary(wire.into())).await.unwrap();
    let _ = ws_sender.send(Message::Close(None)).await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some((_path, len)) = find_capture_file(dir.path()) {
                if len > 0 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_env_max_bytes_drops_frames() {
    let _lock = ENV_LOCK.lock().await;

    let dir = tempfile::tempdir().unwrap();
    let max_bytes = 250u64;
    let _guards = reset_env(dir.path().to_str().unwrap(), &max_bytes.to_string());

    let cfg = ProxyConfig::from_env().unwrap();
    let server = start_server(cfg).await.unwrap();
    let addr = server.local_addr();

    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, _ws_receiver) = ws.split();

    let frame = vec![0u8; 60];
    let wire = aero_l2_protocol::encode_frame(&frame).unwrap();
    for _ in 0..10 {
        ws_sender
            .send(Message::Binary(wire.clone().into()))
            .await
            .unwrap();
    }
    let _ = ws_sender.send(Message::Close(None)).await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some((_path, len)) = find_capture_file(dir.path()) {
                if len > 0 {
                    assert!(
                        len <= max_bytes,
                        "expected capture file to be capped at {max_bytes} bytes, got {len}"
                    );
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let dropped = parse_metric(&body, "l2_capture_frames_dropped_total").unwrap_or(0);
    assert!(
        dropped >= 1,
        "expected dropped capture frames >= 1, got {dropped}"
    );

    server.shutdown().await;
}
