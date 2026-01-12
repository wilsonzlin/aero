#![cfg(not(target_arch = "wasm32"))]

use std::{net::Ipv4Addr, net::SocketAddr, path::PathBuf, time::Duration};

use aero_l2_proxy::{
    start_server, AllowedOrigins, EgressPolicy, ProxyConfig, SecurityConfig, TUNNEL_SUBPROTOCOL,
};
use aero_net_stack::{packet::*, StackConfig};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::{HeaderValue, StatusCode},
    Error as WsError,
};

struct TestServer {
    addr: SocketAddr,
    handle: aero_l2_proxy::ServerHandle,
}

impl TestServer {
    async fn start(capture_dir: Option<PathBuf>, ping_interval: Option<Duration>) -> Self {
        let stack_defaults = StackConfig::default();
        let cfg = ProxyConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            l2_max_frame_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
            l2_max_control_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
            shutdown_grace: Duration::from_millis(3000),
            ping_interval,
            idle_timeout: None,
            tcp_connect_timeout: Duration::from_millis(200),
            tcp_send_buffer: 8,
            ws_send_buffer: 8,
            max_udp_flows_per_tunnel: 256,
            udp_flow_idle_timeout: Some(Duration::from_secs(60)),
            stack_max_tcp_connections: stack_defaults.max_tcp_connections,
            stack_max_pending_dns: stack_defaults.max_pending_dns,
            stack_max_dns_cache_entries: stack_defaults.max_dns_cache_entries,
            stack_max_buffered_tcp_bytes_per_conn: stack_defaults.max_buffered_tcp_bytes_per_conn,
            dns_default_ttl_secs: 60,
            dns_max_ttl_secs: 300,
            capture_dir,
            security: SecurityConfig {
                open: true,
                ..Default::default()
            },
            policy: EgressPolicy::default(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_increment_after_frames() {
    let server = TestServer::start(None, None).await;

    let mut req = server.ws_url().into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
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

    let bytes_rx = parse_metric(&body, "l2_bytes_rx_total").unwrap();
    assert!(
        bytes_rx >= frame.len() as u64,
        "expected bytes rx >= {}, got {bytes_rx}",
        frame.len()
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_increment_after_stack_emits_frames() {
    let server = TestServer::start(None, None).await;

    let body = reqwest::get(server.http_url("/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(parse_metric(&body, "l2_frames_tx_total").unwrap(), 0);
    assert_eq!(parse_metric(&body, "l2_bytes_tx_total").unwrap(), 0);

    let mut req = server.ws_url().into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, mut ws_receiver) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let arp_req = ArpPacketBuilder {
        opcode: ARP_OP_REQUEST,
        sender_mac: guest_mac,
        sender_ip: Ipv4Addr::UNSPECIFIED,
        target_mac: MacAddr([0, 0, 0, 0, 0, 0]),
        target_ip: Ipv4Addr::new(10, 0, 2, 2),
    }
    .build_vec()
    .expect("build ARP request");
    let arp_frame = EthernetFrameBuilder {
        dest_mac: MacAddr::BROADCAST,
        src_mac: guest_mac,
        ethertype: EtherType::ARP,
        payload: &arp_req,
    }
    .build_vec()
    .expect("build ARP Ethernet frame");

    let wire = aero_l2_protocol::encode_frame(&arp_frame).unwrap();
    ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Binary(wire.into()))
        .await
        .unwrap();

    let reply_len = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let msg = ws_receiver
                .next()
                .await
                .expect("ws closed")
                .expect("ws recv");
            let tokio_tungstenite::tungstenite::Message::Binary(buf) = msg else {
                continue;
            };
            let decoded = aero_l2_protocol::decode_message(buf.as_ref()).unwrap();
            if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_FRAME {
                continue;
            }
            let Ok(eth) = EthernetFrame::parse(decoded.payload) else {
                continue;
            };
            if eth.ethertype() != EtherType::ARP {
                continue;
            }
            let Ok(arp) = ArpPacket::parse(eth.payload()) else {
                continue;
            };
            if arp.opcode() == ARP_OP_REPLY {
                break decoded.payload.len();
            }
        }
    })
    .await
    .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(server.http_url("/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let tx = parse_metric(&body, "l2_frames_tx_total").unwrap();
            let bytes_tx = parse_metric(&body, "l2_bytes_tx_total").unwrap();
            if tx >= 1 && bytes_tx >= reply_len as u64 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let _ = ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_active_gauge_tracks_open_tunnels() {
    let server = TestServer::start(None, None).await;

    let body = reqwest::get(server.http_url("/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let active = parse_metric(&body, "l2_sessions_active").unwrap();
    assert_eq!(active, 0, "expected no active sessions at startup");

    let mut req = server.ws_url().into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, mut ws_receiver) = ws.split();

    // Wait for the server to observe the opened session.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(server.http_url("/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let active = parse_metric(&body, "l2_sessions_active").unwrap();
            let total = parse_metric(&body, "l2_sessions_total").unwrap();
            if active == 1 && total >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let _ = ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await;

    // Drain until closed so the server tears down the session.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(msg) = ws_receiver.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await;

    // Wait for the session to be removed from the gauge.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(server.http_url("/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let active = parse_metric(&body, "l2_sessions_active").unwrap();
            if active == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_rejection_metrics_increment_on_missing_origin() {
    let stack_defaults = StackConfig::default();
    let cfg = ProxyConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        l2_max_frame_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
        l2_max_control_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
        shutdown_grace: Duration::from_millis(3000),
        ping_interval: None,
        idle_timeout: None,
        tcp_connect_timeout: Duration::from_millis(200),
        tcp_send_buffer: 8,
        ws_send_buffer: 8,
        max_udp_flows_per_tunnel: 256,
        udp_flow_idle_timeout: Some(Duration::from_secs(60)),
        stack_max_tcp_connections: stack_defaults.max_tcp_connections,
        stack_max_pending_dns: stack_defaults.max_pending_dns,
        stack_max_dns_cache_entries: stack_defaults.max_dns_cache_entries,
        stack_max_buffered_tcp_bytes_per_conn: stack_defaults.max_buffered_tcp_bytes_per_conn,
        dns_default_ttl_secs: 60,
        dns_max_ttl_secs: 300,
        capture_dir: None,
        security: SecurityConfig {
            open: false,
            allowed_origins: AllowedOrigins::Any,
            ..Default::default()
        },
        policy: EgressPolicy::default(),
        test_overrides: Default::default(),
    };

    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected missing Origin to be rejected");

    match err {
        WsError::Http(resp) => assert_eq!(resp.status(), StatusCode::FORBIDDEN),
        other => panic!("expected http error, got {other:?}"),
    }

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let rejected = parse_metric(&body, "l2_upgrade_reject_origin_missing_total").unwrap();
    assert!(
        rejected >= 1,
        "expected origin-missing reject counter >= 1, got {rejected}"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_rejection_metrics_increment_on_origin_not_allowed() {
    let stack_defaults = StackConfig::default();
    let cfg = ProxyConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        l2_max_frame_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
        l2_max_control_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
        shutdown_grace: Duration::from_millis(3000),
        ping_interval: None,
        idle_timeout: None,
        tcp_connect_timeout: Duration::from_millis(200),
        tcp_send_buffer: 8,
        ws_send_buffer: 8,
        max_udp_flows_per_tunnel: 256,
        udp_flow_idle_timeout: Some(Duration::from_secs(60)),
        stack_max_tcp_connections: stack_defaults.max_tcp_connections,
        stack_max_pending_dns: stack_defaults.max_pending_dns,
        stack_max_dns_cache_entries: stack_defaults.max_dns_cache_entries,
        stack_max_buffered_tcp_bytes_per_conn: stack_defaults.max_buffered_tcp_bytes_per_conn,
        dns_default_ttl_secs: 60,
        dns_max_ttl_secs: 300,
        capture_dir: None,
        security: SecurityConfig {
            open: false,
            allowed_origins: AllowedOrigins::List(vec!["https://allowed.test".to_string()]),
            ..Default::default()
        },
        policy: EgressPolicy::default(),
        test_overrides: Default::default(),
    };

    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let ws_url = format!("ws://{addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("origin", HeaderValue::from_static("https://denied.test"));
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected disallowed Origin to be rejected");

    match err {
        WsError::Http(resp) => assert_eq!(resp.status(), StatusCode::FORBIDDEN),
        other => panic!("expected http error, got {other:?}"),
    }

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let rejected = parse_metric(&body, "l2_upgrade_reject_origin_not_allowed_total").unwrap();
    assert!(
        rejected >= 1,
        "expected origin-not-allowed reject counter >= 1, got {rejected}"
    );
    let total = parse_metric(&body, "l2_upgrade_rejected_total").unwrap();
    assert!(
        total >= 1,
        "expected upgrade rejected total >= 1, got {total}"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_rejection_metrics_increment_on_missing_host() {
    let stack_defaults = StackConfig::default();
    let cfg = ProxyConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        l2_max_frame_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
        l2_max_control_payload: aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
        shutdown_grace: Duration::from_millis(3000),
        ping_interval: None,
        idle_timeout: None,
        tcp_connect_timeout: Duration::from_millis(200),
        tcp_send_buffer: 8,
        ws_send_buffer: 8,
        max_udp_flows_per_tunnel: 256,
        udp_flow_idle_timeout: Some(Duration::from_secs(60)),
        stack_max_tcp_connections: stack_defaults.max_tcp_connections,
        stack_max_pending_dns: stack_defaults.max_pending_dns,
        stack_max_dns_cache_entries: stack_defaults.max_dns_cache_entries,
        stack_max_buffered_tcp_bytes_per_conn: stack_defaults.max_buffered_tcp_bytes_per_conn,
        dns_default_ttl_secs: 60,
        dns_max_ttl_secs: 300,
        capture_dir: None,
        security: SecurityConfig {
            open: true,
            allowed_hosts: vec!["allowed.test".to_string()],
            ..Default::default()
        },
        policy: EgressPolicy::default(),
        test_overrides: Default::default(),
    };

    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    // tokio-tungstenite refuses to send a WebSocket upgrade request without a Host header, so send
    // a raw HTTP upgrade request instead.
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(
            format!(
                "GET /l2 HTTP/1.1\r\n\
Connection: Upgrade\r\n\
Upgrade: websocket\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
Sec-WebSocket-Protocol: {TUNNEL_SUBPROTOCOL}\r\n\
\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let mut reader = tokio::io::BufReader::new(reader);
    let mut status_line = String::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut status_line))
        .await
        .unwrap()
        .unwrap();
    assert!(
        status_line.contains(" 403 "),
        "expected 403 response, got: {status_line:?}"
    );

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let rejected = parse_metric(&body, "l2_upgrade_reject_host_missing_total").unwrap();
    assert!(
        rejected >= 1,
        "expected host-missing reject counter >= 1, got {rejected}"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_endpoint_returns_json() {
    let server = TestServer::start(None, None).await;

    let body = reqwest::get(server.http_url("/version"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.trim_start().starts_with('{'),
        "expected JSON object, got: {body}"
    );
    assert!(
        body.contains("\"version\""),
        "missing version field: {body}"
    );
    assert!(body.contains("\"gitSha\""), "missing gitSha field: {body}");
    assert!(
        body.contains("\"builtAt\""),
        "missing builtAt field: {body}"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_creates_non_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(Some(dir.path().to_path_buf()), None).await;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_messages_close_session_with_protocol_error() {
    let server = TestServer::start(None, None).await;

    let mut req = server.ws_url().into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, mut ws_receiver) = ws.split();

    // Send a malformed protocol message (too short / missing header).
    ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            vec![0u8; 1].into(),
        ))
        .await
        .unwrap();

    let mut saw_error = false;
    let mut saw_close = false;
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(msg) = ws_receiver.next().await {
            let msg = msg.unwrap();
            match msg {
                tokio_tungstenite::tungstenite::Message::Binary(buf) => {
                    let decoded = aero_l2_protocol::decode_message(buf.as_ref()).unwrap();
                    if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_ERROR {
                        continue;
                    }
                    let (code, message) =
                        aero_l2_proxy::protocol::decode_error_payload(decoded.payload)
                            .expect("expected structured ERROR payload");
                    assert_eq!(
                        code,
                        aero_l2_proxy::protocol::ERROR_CODE_PROTOCOL_ERROR,
                        "unexpected ERROR code"
                    );
                    assert!(
                        !message.is_empty(),
                        "expected protocol error message to be non-empty"
                    );
                    saw_error = true;
                }
                tokio_tungstenite::tungstenite::Message::Close(frame) => {
                    let frame = frame.expect("expected close frame");
                    assert_eq!(
                        frame.code,
                        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy,
                        "expected close code 1008"
                    );
                    saw_close = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_error, "expected ERROR control message");
    assert!(saw_close, "expected websocket close frame");

    let _ = ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await;

    let body = reqwest::get(server.http_url("/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let dropped = parse_metric(&body, "l2_frames_dropped_total").unwrap();
    assert!(dropped >= 1, "expected dropped counter >= 1, got {dropped}");

    let rx = parse_metric(&body, "l2_frames_rx_total").unwrap();
    assert_eq!(rx, 0, "expected rx counter to remain 0, got {rx}");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ping_rtt_histogram_increments() {
    let server = TestServer::start(None, Some(Duration::from_millis(10))).await;

    let mut req = server.ws_url().into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_sender, mut ws_receiver) = ws.split();

    let ping_payload = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let msg = match ws_receiver.next().await {
                Some(Ok(msg)) => msg,
                Some(Err(err)) => {
                    return Err(std::io::Error::other(err));
                }
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "ws closed",
                    ))
                }
            };

            if let tokio_tungstenite::tungstenite::Message::Binary(buf) = msg {
                let decoded = aero_l2_protocol::decode_message(buf.as_ref()).unwrap();
                if decoded.msg_type == aero_l2_protocol::L2_TUNNEL_TYPE_PING {
                    return Ok(decoded.payload.to_vec());
                }
            }
        }
    })
    .await
    .unwrap()
    .unwrap();

    let pong = aero_l2_protocol::encode_pong(Some(&ping_payload)).unwrap();
    ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Binary(pong.into()))
        .await
        .unwrap();

    let _ = ws_sender
        .send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await;

    tokio::time::sleep(Duration::from_millis(20)).await;

    let body = reqwest::get(server.http_url("/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let count = parse_metric(&body, "l2_ping_rtt_ms_count").unwrap();
    assert!(
        count >= 1,
        "expected ping histogram count >= 1, got {count}"
    );

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
