#![cfg(not(target_arch = "wasm32"))]

use std::{net::SocketAddr, time::Duration};

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

fn metrics_url(addr: SocketAddr) -> String {
    format!("http://{addr}/metrics")
}

async fn fetch_metric(addr: SocketAddr, name: &str) -> u64 {
    let body = reqwest::get(metrics_url(addr))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    parse_metric(&body, name).unwrap_or(0)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_timeout_closes_tunnel_and_increments_metric() {
    let _lock = ENV_LOCK.lock().await;

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _bytes = EnvVarGuard::unset("AERO_L2_MAX_BYTES_PER_CONNECTION");
    let _fps = EnvVarGuard::unset("AERO_L2_MAX_FRAMES_PER_SECOND");
    let _idle = EnvVarGuard::set("AERO_L2_IDLE_TIMEOUT_MS", "50");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let start_metric = fetch_metric(addr, "aero_l2_idle_timeouts_total").await;

    let req = ws_request(addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let close_frame = tokio::time::timeout(Duration::from_secs(2), async {
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

    let frame = close_frame.expect("expected close frame");
    assert_eq!(frame.code, CloseCode::Policy);

    let metric_val = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let val = fetch_metric(addr, "aero_l2_idle_timeouts_total").await;
            if val >= start_metric.saturating_add(1) {
                return val;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        metric_val,
        start_metric.saturating_add(1),
        "expected idle timeout metric to increment once"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_timeout_resets_on_inbound_keepalive() {
    let _lock = ENV_LOCK.lock().await;

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _bytes = EnvVarGuard::unset("AERO_L2_MAX_BYTES_PER_CONNECTION");
    let _fps = EnvVarGuard::unset("AERO_L2_MAX_FRAMES_PER_SECOND");
    let _idle = EnvVarGuard::set("AERO_L2_IDLE_TIMEOUT_MS", "200");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let start_metric = fetch_metric(addr, "aero_l2_idle_timeouts_total").await;

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, mut ws_receiver) = ws.split();

    let ping = aero_l2_protocol::encode_ping(None).unwrap();

    // Send periodic keepalives for longer than the idle timeout and assert the server doesn't
    // close the tunnel.
    let keepalive_duration = Duration::from_millis(600);
    let end_at = tokio::time::Instant::now() + keepalive_duration;
    let mut interval = tokio::time::interval(Duration::from_millis(20));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if tokio::time::Instant::now() >= end_at {
                        break;
                    }
                    ws_sender
                        .send(Message::Binary(ping.clone().into()))
                        .await
                        .expect("ws send ping");
                }
                msg = ws_receiver.next() => {
                    match msg {
                        Some(Ok(Message::Close(frame))) => panic!("unexpected close: {frame:?}"),
                        Some(Ok(_)) => {}
                        Some(Err(err)) => panic!("ws recv error: {err}"),
                        None => panic!("ws closed"),
                    }
                }
            }
        }
    })
    .await
    .unwrap();

    let _ = ws_sender.send(Message::Close(None)).await;

    // Drain the close handshake so the server tears down the session before we fetch metrics.
    // Otherwise, the idle timeout can fire while the test is awaiting the HTTP response.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(msg) = ws_receiver.next().await {
            match msg {
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                // Some servers may reset the TCP socket after sending a close frame (or without a
                // close handshake). Treat this as "closed" for this test.
                Err(_) => break,
            }
        }
    })
    .await;

    let val = fetch_metric(addr, "aero_l2_idle_timeouts_total").await;
    assert_eq!(val, start_metric, "expected no idle timeout closures");

    proxy.shutdown().await;
}
