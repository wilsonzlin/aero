#![cfg(not(target_arch = "wasm32"))]

use std::{net::SocketAddr, time::Duration};

use aero_l2_proxy::{start_server, EgressPolicy, ProxyConfig, SecurityConfig, TUNNEL_SUBPROTOCOL};
use aero_net_stack::StackConfig;
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::HeaderValue,
    protocol::{frame::coding::CloseCode, Message},
};

fn test_config(bind_addr: SocketAddr, shutdown_grace: Duration) -> ProxyConfig {
    let stack_defaults = StackConfig::default();
    ProxyConfig {
        bind_addr,
        l2_max_frame_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
        l2_max_control_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
        shutdown_grace,
        ping_interval: None,
        idle_timeout: None,
        tcp_connect_timeout: Duration::from_millis(200),
        tcp_send_buffer: 8,
        ws_send_buffer: 8,
        max_udp_flows_per_tunnel: 256,
        udp_flow_idle_timeout: Some(Duration::from_millis(60_000)),
        stack_max_tcp_connections: stack_defaults.max_tcp_connections,
        stack_max_pending_dns: stack_defaults.max_pending_dns,
        stack_max_dns_cache_entries: stack_defaults.max_dns_cache_entries,
        stack_max_buffered_tcp_bytes_per_conn: stack_defaults.max_buffered_tcp_bytes_per_conn,
        dns_default_ttl_secs: 60,
        dns_max_ttl_secs: 300,
        capture_dir: None,
        security: SecurityConfig {
            open: true,
            ..Default::default()
        },
        policy: EgressPolicy::default(),
        test_overrides: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readyz_drains_on_shutdown() {
    let cfg = test_config(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        Duration::from_millis(3000),
    );
    let handle = start_server(cfg).await.unwrap();
    let addr = handle.local_addr();

    let url = format!("http://{addr}/readyz");
    let status = reqwest::get(url.clone()).await.unwrap().status();
    assert_eq!(status, reqwest::StatusCode::OK);

    handle.mark_shutting_down();
    let status = reqwest::get(url).await.unwrap().status();
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_tunnels_close_on_shutdown_and_server_exits() {
    let shutdown_grace = Duration::from_millis(2000);
    let cfg = test_config(SocketAddr::from(([127, 0, 0, 1], 0)), shutdown_grace);
    let handle = start_server(cfg).await.unwrap();
    let addr = handle.local_addr();

    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );

    let (ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    let mut ws = ws;

    let close_watch = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match ws.next().await {
                    Some(Ok(Message::Close(frame))) => return frame,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) | None => return None,
                }
            }
        })
        .await
    });

    handle.mark_shutting_down();

    let started = tokio::time::Instant::now();
    handle.shutdown().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed <= shutdown_grace + Duration::from_millis(250),
        "shutdown took {elapsed:?}, expected <= {shutdown_grace:?}",
    );

    let close = close_watch.await.unwrap().unwrap();
    if let Some(frame) = close {
        assert_eq!(frame.code, CloseCode::Away);
        assert_eq!(frame.reason, "shutting down");
    }
}
