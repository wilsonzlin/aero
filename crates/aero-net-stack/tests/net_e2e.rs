#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::HashMap,
    io::Cursor,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use aero_net_stack::packet::*;
use aero_net_stack::{
    Action, DnsResolved, NetworkStack, StackConfig, TcpProxyEvent, UdpProxyEvent,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::oneshot,
    task::JoinHandle,
    time::{timeout, Duration},
};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

/// End-to-end integration test that replaces the legacy `crates/aero-net` `net_e2e`:
/// - guest DHCP + ARP
/// - guest DNS query resolved via DoH (embedded test server)
/// - guest TCP stream proxied via the current `/tcp` WebSocket contract (Aero Gateway;
///   `GET /tcp?v=1&host=<host>&port=<port>`; legacy `target=<host>:<port>` is also accepted)
/// - guest UDP datagram proxied via host-side UDP relay (test harness)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn net_e2e() {
    let tcp_echo = TcpEchoServer::spawn().await;
    let udp_echo = UdpEchoServer::spawn().await;
    let doh = DohHttpServer::spawn(HashMap::from([(
        "echo.local".to_string(),
        Ipv4Addr::new(127, 0, 0, 1),
    )]))
    .await;
    let tcp_relay = TcpWsRelayServer::spawn().await;

    // The canonical gateway contract requires the session cookie; ensure missing cookies are
    // rejected before we bootstrap a session.
    assert_doh_rejects_without_cookie(doh.addr).await;
    assert_tcp_relay_rejects_without_cookie(
        tcp_relay.addr,
        Ipv4Addr::LOCALHOST,
        tcp_echo.addr.port(),
    )
    .await;

    let session_cookie = create_gateway_session(tcp_relay.addr).await;
    assert_tcp_relay_rejects_invalid_target_over_host_port(
        tcp_relay.addr,
        &session_cookie,
        Ipv4Addr::LOCALHOST,
        tcp_echo.addr.port(),
    )
    .await;
    assert_tcp_relay_accepts_target_over_invalid_host_port(
        tcp_relay.addr,
        &session_cookie,
        Ipv4Addr::LOCALHOST,
        tcp_echo.addr.port(),
    )
    .await;

    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);

    // --- DHCP handshake (establish guest IP) ---
    dhcp_handshake(&mut stack, guest_mac);

    // --- ARP (guest resolves gateway MAC) ---
    let arp_payload = ArpPacketBuilder {
        opcode: ARP_OP_REQUEST,
        sender_mac: guest_mac,
        sender_ip: stack.config().guest_ip,
        target_mac: MacAddr([0, 0, 0, 0, 0, 0]),
        target_ip: stack.config().gateway_ip,
    }
    .build_vec()
    .unwrap();
    let arp_frame = EthernetFrameBuilder {
        dest_mac: MacAddr::BROADCAST,
        src_mac: guest_mac,
        ethertype: EtherType::ARP,
        payload: &arp_payload,
    }
    .build_vec()
    .unwrap();
    let actions = stack.process_outbound_ethernet(&arp_frame, 10);
    let arp_resp_frame = extract_single_frame(&actions);
    let eth = EthernetFrame::parse(&arp_resp_frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::ARP);
    assert_eq!(eth.src_mac(), stack.config().our_mac);
    assert_eq!(eth.dest_mac(), guest_mac);
    let arp_resp = ArpPacket::parse(eth.payload()).unwrap();
    assert_eq!(arp_resp.opcode(), ARP_OP_REPLY);
    assert_eq!(arp_resp.sender_ip(), Some(stack.config().gateway_ip));
    assert_eq!(arp_resp.sender_mac(), Some(stack.config().our_mac));

    // --- Enable outbound networking policy ---
    stack.set_network_enabled(true);

    // --- DNS query (guest -> stack -> DoH -> stack -> guest) ---
    let dns_txid = 0x1234;
    let dns_query = build_dns_query(dns_txid, "echo.local", DnsType::A as u16);
    let dns_frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        53000,
        53,
        &dns_query,
    );
    let actions = stack.process_outbound_ethernet(&dns_frame, 20);
    let (dns_req_id, dns_name) = match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        _ => panic!("expected single DnsResolve action, got {actions:?}"),
    };
    assert_eq!(dns_name, "echo.local");

    let resolved_ip = resolve_via_doh(doh.addr, &session_cookie, &dns_name)
        .await
        .expect("doh resolved echo.local");
    assert_eq!(resolved_ip, Ipv4Addr::new(127, 0, 0, 1));

    let dns_actions = stack.handle_dns_resolved(
        DnsResolved {
            request_id: dns_req_id,
            name: dns_name,
            addr: Some(resolved_ip),
            ttl_secs: 60,
        },
        21,
    );
    let dns_resp_frame = extract_single_frame(&dns_actions);
    assert_dns_response_has_a_record(&dns_resp_frame, dns_txid, resolved_ip.octets());

    // --- TCP connect + echo (stack <-> /tcp WS relay <-> local echo server) ---
    let remote_ip = resolved_ip;
    let remote_port = tcp_echo.addr.port();
    let guest_port = 40001;
    let guest_isn = 5000;

    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn, 30);
    let (conn_id, syn_ack_frame) = extract_tcp_connect_and_frame(&actions);
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame);
    assert_eq!(syn_ack.ack_number(), guest_isn + 1);

    let mut tcp_proxy = timeout(
        Duration::from_secs(2),
        TcpProxyClient::connect(tcp_relay.addr, &session_cookie, remote_ip, remote_port),
    )
    .await
    .expect("tcp relay connect timeout")
    .expect("tcp relay connect");
    assert!(stack
        .handle_tcp_proxy_event(
            TcpProxyEvent::Connected {
                connection_id: conn_id
            },
            31
        )
        .is_empty());

    // Guest ACK to complete handshake.
    let ack = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_isn + 1,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK,
        &[],
    );
    assert!(stack.process_outbound_ethernet(&ack, 32).is_empty());

    // Guest sends application data.
    let payload = b"hello over proxy";
    let psh = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_isn + 1,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    let actions = stack.process_outbound_ethernet(&psh, 33);
    let to_proxy = match actions.as_slice() {
        [Action::TcpProxySend {
            connection_id,
            data,
        }, Action::EmitFrame(_)]
        | [Action::EmitFrame(_), Action::TcpProxySend {
            connection_id,
            data,
        }] => {
            assert_eq!(*connection_id, conn_id);
            data.clone()
        }
        _ => panic!("expected TcpProxySend + EmitFrame, got {actions:?}"),
    };

    tcp_proxy.send_binary(&to_proxy).await.expect("proxy send");
    let echoed = timeout(Duration::from_secs(2), tcp_proxy.recv_exact(to_proxy.len()))
        .await
        .expect("proxy recv timeout")
        .expect("proxy recv");

    let actions = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Data {
            connection_id: conn_id,
            data: echoed.clone(),
        },
        34,
    );
    let inbound = extract_single_frame(&actions);
    let seg = parse_tcp_from_frame(&inbound);
    assert_eq!(seg.src_port(), remote_port);
    assert_eq!(seg.dst_port(), guest_port);
    assert_eq!(seg.payload(), echoed);

    // --- UDP roundtrip (stack -> host UDP send -> stack -> guest) ---
    let udp_guest_port = 50000;
    let udp_payload = b"ping";
    let udp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        udp_guest_port,
        udp_echo.addr.port(),
        udp_payload,
    );
    let actions = stack.process_outbound_ethernet(&udp_frame, 40);
    let (udp_dst_ip, udp_dst_port) = match actions.as_slice() {
        [Action::UdpProxySend {
            transport: _,
            src_port,
            dst_ip,
            dst_port,
            data,
        }] => {
            assert_eq!(*src_port, udp_guest_port);
            assert_eq!(data.as_slice(), udp_payload);
            (*dst_ip, *dst_port)
        }
        _ => panic!("expected UdpProxySend, got {actions:?}"),
    };
    assert_eq!(udp_dst_ip, remote_ip);

    let echoed_udp = timeout(
        Duration::from_secs(2),
        udp_send_recv(
            SocketAddr::new(udp_dst_ip.into(), udp_dst_port),
            udp_payload,
        ),
    )
    .await
    .expect("udp roundtrip timeout")
    .expect("udp roundtrip failed");
    assert_eq!(echoed_udp.as_slice(), udp_payload);

    let actions = stack.handle_udp_proxy_event(
        UdpProxyEvent {
            src_ip: udp_dst_ip,
            src_port: udp_dst_port,
            dst_port: udp_guest_port,
            data: echoed_udp,
        },
        41,
    );
    let inbound = extract_single_frame(&actions);
    let udp = parse_udp_from_frame(&inbound);
    assert_eq!(udp.src_port(), udp_dst_port);
    assert_eq!(udp.dst_port(), udp_guest_port);
    assert_eq!(udp.payload(), udp_payload);

    tcp_proxy.shutdown().await;
    tcp_relay.shutdown().await;
    doh.shutdown().await;
    udp_echo.shutdown().await;
    tcp_echo.shutdown().await;
}

struct TcpEchoServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl TcpEchoServer {
    async fn spawn() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind tcp echo");
        let addr = listener.local_addr().expect("tcp echo local addr");
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let (mut stream, _) = match accept {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 16 * 1024];
                            loop {
                                let n = match stream.read(&mut buf).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => n,
                                };
                                if stream.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                }
            }
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            task,
        }
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

struct UdpEchoServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl UdpEchoServer {
    async fn spawn() -> Self {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind udp echo");
        let addr = socket.local_addr().expect("udp echo local addr");
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let task = tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    res = socket.recv_from(&mut buf) => {
                        let (n, peer) = match res {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let _ = socket.send_to(&buf[..n], peer).await;
                    }
                }
            }
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            task,
        }
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

struct DohHttpServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl DohHttpServer {
    async fn spawn(records: HashMap<String, Ipv4Addr>) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind doh");
        let addr = listener.local_addr().expect("doh local addr");
        let records = Arc::new(records);

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let (stream, _) = match accept {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let records = records.clone();
                        tokio::spawn(async move {
                            let _ = handle_doh_connection(stream, records).await;
                        });
                    }
                }
            }
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            task,
        }
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

async fn handle_doh_connection(
    mut stream: TcpStream,
    records: Arc<HashMap<String, Ipv4Addr>>,
) -> std::io::Result<()> {
    let (method, path, headers, body) = read_http_request(&mut stream).await?;
    if method != "POST" || path != "/dns-query" {
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }

    // Mirror the canonical gateway contract: DoH requires the session cookie.
    let cookie_ok = headers
        .get("cookie")
        .map(|v| v.as_str())
        .unwrap_or("")
        .split(';')
        .any(|part| {
            let part = part.trim();
            let Some((k, v)) = part.split_once('=') else {
                return false;
            };
            k.trim() == "aero_session" && v.trim() == TEST_AERO_SESSION
        });
    if !cookie_ok {
        stream
            .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }

    let content_type = headers
        .get("content-type")
        .map(|v| v.as_str())
        .unwrap_or("");
    if !content_type.starts_with("application/dns-message") {
        stream
            .write_all(b"HTTP/1.1 415 Unsupported Media Type\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }

    let query = parse_single_query(&body)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid dns query"))?;
    let name = qname_to_string(query.qname)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid qname"))?
        .to_ascii_lowercase();
    let (answer_a, rcode) = match records.get(&name) {
        Some(ip) => (Some(*ip), DnsResponseCode::NoError),
        None => (None, DnsResponseCode::NameError),
    };
    let resp = DnsResponseBuilder {
        id: query.id,
        rd: query.recursion_desired(),
        rcode,
        qname: query.qname,
        qtype: query.qtype,
        qclass: query.qclass,
        answer_a,
        ttl: 60,
    }
    .build_vec()
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "dns response build"))?;

    let mut out = Vec::new();
    out.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
    out.extend_from_slice(b"Content-Type: application/dns-message\r\n");
    out.extend_from_slice(format!("Content-Length: {}\r\n", resp.len()).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n");
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&resp);
    stream.write_all(&out).await?;
    Ok(())
}

async fn resolve_via_doh(addr: SocketAddr, session_cookie: &str, name: &str) -> Option<Ipv4Addr> {
    let txid = 0xBEEF;
    let query = build_dns_query(txid, name, DnsType::A as u16);

    let mut stream = TcpStream::connect(addr).await.ok()?;

    let mut req = Vec::new();
    req.extend_from_slice(b"POST /dns-query HTTP/1.1\r\n");
    req.extend_from_slice(b"Host: localhost\r\n");
    req.extend_from_slice(format!("Cookie: aero_session={session_cookie}\r\n").as_bytes());
    req.extend_from_slice(b"Content-Type: application/dns-message\r\n");
    req.extend_from_slice(b"Accept: application/dns-message\r\n");
    req.extend_from_slice(format!("Content-Length: {}\r\n", query.len()).as_bytes());
    req.extend_from_slice(b"Connection: close\r\n");
    req.extend_from_slice(b"\r\n");
    req.extend_from_slice(&query);

    stream.write_all(&req).await.ok()?;
    let (_proto, status, _headers, body) = read_http_response(&mut stream).await.ok()?;
    if status != "200" {
        return None;
    }

    if body.len() < 12 {
        return None;
    }
    // ANCOUNT == 1 implies the A RDATA is the final 4 bytes for our minimal responses.
    let ancount = u16::from_be_bytes([body[6], body[7]]);
    if ancount != 1 || body.len() < 4 {
        return None;
    }
    let ip = &body[body.len() - 4..];
    Some(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]))
}

async fn udp_send_recv(dst: SocketAddr, payload: &[u8]) -> std::io::Result<Vec<u8>> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    socket.send_to(payload, dst).await?;
    let mut buf = vec![0u8; 64 * 1024];
    let (n, _peer) = socket.recv_from(&mut buf).await?;
    buf.truncate(n);
    Ok(buf)
}

const TEST_AERO_SESSION: &str = "test-session";

async fn create_gateway_session(addr: SocketAddr) -> String {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("connect tcp relay /session");

    // Mirror the canonical gateway bootstrap: `POST /session` sets `aero_session`.
    let req = b"POST /session HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    stream
        .write_all(req)
        .await
        .expect("send tcp relay /session request");

    let (_proto, status, headers, body) = read_http_response(&mut stream)
        .await
        .expect("read tcp relay /session response");
    assert_eq!(status, "201", "expected 201 Created from /session");

    assert!(
        !body.is_empty(),
        "expected non-empty json body from /session"
    );
    let payload = std::str::from_utf8(&body).expect("tcp relay /session json is utf-8");
    assert!(
        payload.contains("\"tcp\":\"/tcp\""),
        "expected /session response to advertise endpoints.tcp"
    );
    assert!(
        payload.contains("\"dnsQuery\":\"/dns-query\""),
        "expected /session response to advertise endpoints.dnsQuery"
    );

    let set_cookie = headers
        .get("set-cookie")
        .expect("tcp relay /session missing Set-Cookie");
    parse_set_cookie_value(set_cookie, "aero_session").expect("parse aero_session cookie")
}

async fn assert_tcp_relay_rejects_without_cookie(
    addr: SocketAddr,
    remote_ip: Ipv4Addr,
    remote_port: u16,
) {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("connect tcp relay /tcp (missing cookie)");

    let req = format!(
        "GET /tcp?v=1&host={remote_ip}&port={remote_port} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .expect("send tcp relay /tcp (missing cookie)");

    let (_proto, status, _headers, _body) = read_http_response(&mut stream)
        .await
        .expect("read tcp relay /tcp response");
    assert_eq!(status, "401", "expected 401 Unauthorized from /tcp");
}

async fn assert_doh_rejects_without_cookie(addr: SocketAddr) {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("connect doh /dns-query (missing cookie)");

    let req = b"POST /dns-query HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    stream
        .write_all(req)
        .await
        .expect("send doh /dns-query (missing cookie)");

    let (_proto, status, _headers, _body) = read_http_response(&mut stream)
        .await
        .expect("read doh /dns-query response");
    assert_eq!(status, "401", "expected 401 Unauthorized from /dns-query");
}

async fn assert_tcp_relay_rejects_invalid_target_over_host_port(
    addr: SocketAddr,
    session_cookie: &str,
    remote_ip: Ipv4Addr,
    remote_port: u16,
) {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("connect tcp relay /tcp (invalid target)");

    // Gateway semantics: if `target` is present, it takes precedence over host/port. That means an
    // invalid target must fail the request even if host/port are valid.
    let req = format!(
        "GET /tcp?v=1&host={remote_ip}&port={remote_port}&target= HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nCookie: aero_session={session_cookie}\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .expect("send tcp relay /tcp (invalid target)");

    let (_proto, status, _headers, _body) = read_http_response(&mut stream)
        .await
        .expect("read tcp relay /tcp response");
    assert_eq!(status, "400", "expected 400 Bad Request from /tcp");
}

async fn assert_tcp_relay_accepts_target_over_invalid_host_port(
    addr: SocketAddr,
    session_cookie: &str,
    remote_ip: Ipv4Addr,
    remote_port: u16,
) {
    // Use an invalid host/port pair, but a valid `target=`. The relay should ignore host/port and
    // accept the connection based on `target=`.
    let url = format!("ws://{addr}/tcp?v=1&target={remote_ip}:{remote_port}&host=&port=0");
    let mut request = url.into_client_request().expect("build tcp relay request");
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::COOKIE,
        HeaderValue::from_str(&format!("aero_session={session_cookie}"))
            .expect("valid Cookie header value"),
    );
    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("tcp relay should accept target over invalid host/port");
    let _ = ws.send(Message::Close(None)).await;
}

fn parse_set_cookie_value(set_cookie: &str, cookie_name: &str) -> Option<String> {
    let first = set_cookie.split(';').next()?.trim();
    let (name, value) = first.split_once('=')?;
    (name.trim() == cookie_name).then(|| value.trim().to_string())
}

/// Minimal TCP relay implementing the `/tcp` WebSocket contract.
///
/// Supports:
/// - Canonical gateway format: `GET /tcp?v=1&host=<host>&port=<port>` (host also accepts bracketed
///   IPv6, e.g. `host=[::1]`).
/// - Legacy format: `GET /tcp?target=<host>:<port>` (also supports bracketed IPv6)
///
/// Also supports a minimal `POST /session` endpoint that issues an `aero_session` cookie, and
/// requires that cookie on `/tcp` upgrades (mirroring the canonical gateway contract).
struct TcpWsRelayServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl TcpWsRelayServer {
    async fn spawn() -> Self {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind tcp relay");
        let addr = listener.local_addr().expect("tcp relay local addr");

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            serve_tcp_relay(listener, shutdown_rx).await;
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            task,
        }
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

async fn serve_tcp_relay(
    listener: tokio::net::TcpListener,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accept = listener.accept() => {
                let (stream, _peer) = match accept {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                tokio::spawn(async move {
                    let _ = handle_tcp_relay_client(stream).await;
                });
            }
        }
    }
}

struct ReplayStream {
    inner: TcpStream,
    replay: Cursor<Vec<u8>>,
}

impl ReplayStream {
    fn new(inner: TcpStream, replay: Vec<u8>) -> Self {
        Self {
            inner,
            replay: Cursor::new(replay),
        }
    }
}

impl AsyncRead for ReplayStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        let pos = me.replay.position() as usize;
        let replay_buf = me.replay.get_ref();
        if pos < replay_buf.len() {
            let remaining = &replay_buf[pos..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            me.replay.set_position((pos + to_copy) as u64);
            return Poll::Ready(Ok(()));
        }

        Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for ReplayStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, data)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

async fn handle_tcp_relay_client(mut stream: TcpStream) -> std::io::Result<()> {
    use tokio_tungstenite::tungstenite::http::Response;

    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end;
    loop {
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http header too large",
            ));
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof while reading http header",
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
    }

    let header = std::str::from_utf8(&buf[..header_end])
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid http header"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();

    if method == "POST" && path == "/session" {
        let body = json!({
            "session": { "expiresAt": "2099-01-01T00:00:00Z" },
            "endpoints": {
                "tcp": "/tcp",
                "tcpMux": "/tcp-mux",
                "dnsQuery": "/dns-query",
                "dnsJson": "/dns-json",
                "l2": "/l2",
                "udpRelayToken": "/udp-relay/token"
            },
            "limits": {
                "tcp": { "maxConnections": 64, "maxMessageBytes": 1048576, "connectTimeoutMs": 10000, "idleTimeoutMs": 300000 },
                "dns": { "maxQueryBytes": 4096 },
                "l2": {
                    "maxFramePayloadBytes": aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
                    "maxControlPayloadBytes": aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD
                }
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 201 Created\r\nSet-Cookie: aero_session={TEST_AERO_SESSION}; Path=/; HttpOnly\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    let replay = ReplayStream::new(stream, buf);

    let mut target: Option<(String, u16)> = None;
    let ws_stream = tokio_tungstenite::accept_hdr_async(
        replay,
        |req: &tokio_tungstenite::tungstenite::http::Request<()>, resp| {
            let uri = req.uri();
            if uri.path() != "/tcp" {
                return Err(Response::builder()
                    .status(404)
                    .body(Some("invalid path".to_string()))
                    .expect("build response"));
            }

            let cookie_ok = req
                .headers()
                .get(tokio_tungstenite::tungstenite::http::header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .split(';')
                .any(|part| {
                    let part = part.trim();
                    let Some((k, v)) = part.split_once('=') else {
                        return false;
                    };
                    k.trim() == "aero_session" && v.trim() == TEST_AERO_SESSION
                });

            if !cookie_ok {
                return Err(Response::builder()
                    .status(401)
                    .body(Some("missing aero_session cookie".to_string()))
                    .expect("build response"));
            }

            let mut raw_version: Option<String> = None;
            let mut raw_host: Option<String> = None;
            let mut raw_port: Option<String> = None;
            let mut raw_target: Option<String> = None;

            if let Some(query) = uri.query() {
                for pair in query.split('&') {
                    let (k_raw, v_raw) = pair.split_once('=').unwrap_or((pair, ""));
                    let k = url_decode_component(k_raw).unwrap_or_else(|| k_raw.to_string());
                    let v = url_decode_component(v_raw).unwrap_or_else(|| v_raw.to_string());
                    match k.as_str() {
                        // Match WHATWG URLSearchParams.get() behavior: return the first value for a
                        // given key when duplicates are present.
                        "v" => {
                            if raw_version.is_none() {
                                raw_version = Some(v);
                            }
                        }
                        "host" => {
                            if raw_host.is_none() {
                                raw_host = Some(v);
                            }
                        }
                        "port" => {
                            if raw_port.is_none() {
                                raw_port = Some(v);
                            }
                        }
                        "target" => {
                            if raw_target.is_none() {
                                raw_target = Some(v);
                            }
                        }
                        _ => {}
                    }
                }
            }

            if let Some(raw) = raw_version.as_deref() {
                // Mirror the gateway behavior: v defaults to 1 if omitted/empty, and any non-1
                // value is rejected.
                if !raw.is_empty() && raw != "1" {
                    return Err(Response::builder()
                        .status(400)
                        .body(Some("unsupported tcp version".to_string()))
                        .expect("build response"));
                }
            }

            // Match the documented gateway precedence: if both the canonical form and the legacy
            // `target=` alias are provided, prefer `target`.
            if let Some(raw_target) = raw_target {
                target = match parse_target(&raw_target) {
                    Ok(v) => Some(v),
                    Err(_) => {
                        return Err(Response::builder()
                            .status(400)
                            .body(Some("invalid target".to_string()))
                            .expect("build response"));
                    }
                };
            } else {
                let Some(raw_host) = raw_host else {
                    return Err(Response::builder()
                        .status(400)
                        .body(Some("missing host".to_string()))
                        .expect("build response"));
                };
                let Some(raw_port) = raw_port else {
                    return Err(Response::builder()
                        .status(400)
                        .body(Some("missing port".to_string()))
                        .expect("build response"));
                };

                let host = match parse_host(&raw_host) {
                    Ok(v) => v,
                    Err(_) => {
                        return Err(Response::builder()
                            .status(400)
                            .body(Some("invalid host".to_string()))
                            .expect("build response"));
                    }
                };
                let port = match parse_port(&raw_port) {
                    Ok(v) => v,
                    Err(_) => {
                        return Err(Response::builder()
                            .status(400)
                            .body(Some("invalid port".to_string()))
                            .expect("build response"));
                    }
                };
                if port == 0 {
                    return Err(Response::builder()
                        .status(400)
                        .body(Some("invalid port".to_string()))
                        .expect("build response"));
                }
                target = Some((host, port));
            }

            if target.is_none() {
                return Err(Response::builder()
                    .status(400)
                    .body(Some("missing target".to_string()))
                    .expect("build response"));
            }

            Ok(resp)
        },
    )
    .await
    .map_err(|err| std::io::Error::other(err.to_string()))?;

    let (host, port) = target.expect("validated during handshake");
    let tcp = TcpStream::connect((host.as_str(), port)).await?;

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (mut tcp_reader, mut tcp_writer) = tcp.into_split();

    let c2t = async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Binary(data) => {
                    if tcp_writer.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = tcp_writer.shutdown().await;
    };

    let t2c = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = match tcp_reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };

            if ws_sender
                .send(Message::Binary(buf[..n].to_vec().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    };

    tokio::join!(c2t, t2c);
    Ok(())
}

fn parse_target(target: &str) -> Result<(String, u16), &'static str> {
    if target.is_empty() {
        return Err("empty target");
    }
    if let Some(rest) = target.strip_prefix('[') {
        let Some((host, rest)) = rest.split_once(']') else {
            return Err("missing closing bracket in IPv6 address");
        };
        if host.is_empty() {
            return Err("missing host");
        }
        let Some(port) = rest.strip_prefix(':') else {
            return Err("missing :port suffix");
        };
        let port = parse_port(port)?;
        return Ok((host.to_string(), port));
    }

    let Some((host, port)) = target.rsplit_once(':') else {
        return Err("missing :port suffix");
    };
    if host.is_empty() {
        return Err("missing host");
    }
    // Match the canonical gateway behavior: `target=<host>:<port>` requires bracketed IPv6
    // (otherwise the host/port split is ambiguous).
    if host.contains(':') {
        return Err("IPv6 targets must be bracketed");
    }
    if host.contains('[') || host.contains(']') {
        return Err("invalid target: unexpected bracket in host");
    }
    let port = parse_port(port)?;
    Ok((host.to_string(), port))
}

fn parse_host(host: &str) -> Result<String, &'static str> {
    if host.is_empty() {
        return Err("missing host");
    }
    if let Some(rest) = host.strip_prefix('[') {
        let Some(host) = rest.strip_suffix(']') else {
            return Err("missing closing bracket in host");
        };
        if host.is_empty() {
            return Err("missing host");
        }
        return Ok(host.to_string());
    }
    if host.ends_with(']') {
        return Err("mismatched host brackets");
    }
    Ok(host.to_string())
}

fn parse_port(port: &str) -> Result<u16, &'static str> {
    if port.is_empty() {
        return Err("invalid port");
    }
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid port");
    }
    let port: u16 = port.parse().map_err(|_| "invalid port")?;
    if port == 0 {
        return Err("invalid port");
    }
    Ok(port)
}

fn url_decode_component(input: &str) -> Option<String> {
    fn from_hex_digit(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    let bytes = input.as_bytes();
    let mut out = Vec::<u8>::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = from_hex_digit(bytes[i + 1])?;
                let lo = from_hex_digit(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b'+' => {
                // WHATWG URLSearchParams uses application/x-www-form-urlencoded semantics.
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }

    String::from_utf8(out).ok()
}

struct TcpProxyClient {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl TcpProxyClient {
    async fn connect(
        proxy_addr: SocketAddr,
        session_cookie: &str,
        remote_ip: Ipv4Addr,
        remote_port: u16,
    ) -> Result<Self, tokio_tungstenite::tungstenite::Error> {
        let url = format!("ws://{proxy_addr}/tcp?v=1&host={remote_ip}&port={remote_port}");
        let mut request = url.into_client_request()?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::COOKIE,
            HeaderValue::from_str(&format!("aero_session={session_cookie}"))
                .expect("valid Cookie header value"),
        );
        let (ws, _resp) = tokio_tungstenite::connect_async(request).await?;
        Ok(Self { ws })
    }

    async fn send_binary(
        &mut self,
        data: &[u8],
    ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        self.ws.send(Message::Binary(data.to_vec().into())).await?;
        Ok(())
    }

    async fn recv_exact(
        &mut self,
        n: usize,
    ) -> Result<Vec<u8>, tokio_tungstenite::tungstenite::Error> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            match self.ws.next().await {
                Some(Ok(Message::Binary(chunk))) => out.extend_from_slice(&chunk),
                Some(Ok(Message::Close(_))) | None => {
                    return Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed);
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err),
            }
        }
        if out.len() > n {
            out.truncate(n);
        }
        Ok(out)
    }

    async fn shutdown(mut self) {
        let _ = self.ws.send(Message::Close(None)).await;
    }
}

async fn read_http_request(
    stream: &mut TcpStream,
) -> std::io::Result<(String, String, HashMap<String, String>, Vec<u8>)> {
    read_http_message(stream).await
}

async fn read_http_response(
    stream: &mut TcpStream,
) -> std::io::Result<(String, String, HashMap<String, String>, Vec<u8>)> {
    read_http_message(stream).await
}

async fn read_http_message(
    stream: &mut TcpStream,
) -> std::io::Result<(String, String, HashMap<String, String>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end;
    loop {
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http header too large",
            ));
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof while reading http header",
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
    }

    let header = std::str::from_utf8(&buf[..header_end])
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid http header"))?;
    let mut lines = header.split("\r\n");
    let start_line = lines.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing start line")
    })?;
    let mut start_parts = start_line.split_whitespace();
    let method = start_parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing method"))?
        .to_string();
    let path = start_parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing path"))?
        .to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let content_len = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_len {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_len);

    Ok((method, path, headers, body))
}

fn dhcp_handshake(stack: &mut NetworkStack, guest_mac: MacAddr) {
    let xid = 0x1020_3040;
    let discover = build_dhcp_discover(xid, guest_mac);
    let discover_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &discover,
    );

    let actions = stack.process_outbound_ethernet(&discover_frame, 0);
    let offer_frames = extract_frames(&actions);
    assert_eq!(offer_frames.len(), 2);
    let offer_msg = parse_dhcp_from_frame(&offer_frames[0]);
    assert_eq!(offer_msg.message_type, DhcpMessageType::Offer);

    let request = build_dhcp_request(
        xid,
        guest_mac,
        stack.config().guest_ip,
        stack.config().gateway_ip,
    );
    let request_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &request,
    );
    let actions = stack.process_outbound_ethernet(&request_frame, 1);
    let ack_frames = extract_frames(&actions);
    assert_eq!(ack_frames.len(), 2);
    let ack_msg = parse_dhcp_from_frame(&ack_frames[0]);
    assert_eq!(ack_msg.message_type, DhcpMessageType::Ack);
    assert!(stack.is_ip_assigned());
}

fn extract_single_frame(actions: &[Action]) -> Vec<u8> {
    let frames = extract_frames(actions);
    assert_eq!(frames.len(), 1, "expected 1 EmitFrame, got {actions:?}");
    frames.into_iter().next().unwrap()
}

fn extract_frames(actions: &[Action]) -> Vec<Vec<u8>> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::EmitFrame(f) => Some(f.clone()),
            _ => None,
        })
        .collect()
}

fn extract_tcp_connect_and_frame(actions: &[Action]) -> (u32, Vec<u8>) {
    let mut conn_id = None;
    let mut frame = None;
    for a in actions {
        match a {
            Action::TcpProxyConnect {
                connection_id,
                remote_ip: _,
                remote_port: _,
            } => conn_id = Some(*connection_id),
            Action::EmitFrame(f) => frame = Some(f.clone()),
            _ => {}
        }
    }
    (
        conn_id.expect("missing TcpProxyConnect"),
        frame.expect("missing EmitFrame"),
    )
}

fn parse_dhcp_from_frame(frame: &[u8]) -> DhcpMessage {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    assert_eq!(ip.protocol(), Ipv4Protocol::UDP);
    let udp = UdpDatagram::parse(ip.payload()).unwrap();
    assert_eq!(udp.src_port(), 67);
    assert_eq!(udp.dst_port(), 68);
    DhcpMessage::parse(udp.payload()).unwrap()
}

fn parse_tcp_from_frame(frame: &[u8]) -> TcpSegment<'_> {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    assert_eq!(ip.protocol(), Ipv4Protocol::TCP);
    TcpSegment::parse(ip.payload()).unwrap()
}

fn parse_udp_from_frame(frame: &[u8]) -> UdpDatagram<'_> {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    assert_eq!(ip.protocol(), Ipv4Protocol::UDP);
    UdpDatagram::parse(ip.payload()).unwrap()
}

fn assert_dns_response_has_a_record(frame: &[u8], id: u16, addr: [u8; 4]) {
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    let udp = UdpDatagram::parse(ip.payload()).unwrap();
    assert_eq!(udp.src_port(), 53);
    let dns = udp.payload();
    assert_eq!(&dns[0..2], &id.to_be_bytes());
    // QR=1
    assert_eq!(dns[2] & 0x80, 0x80);
    // ANCOUNT == 1
    assert_eq!(&dns[6..8], &1u16.to_be_bytes());
    // Answer RDATA is the final 4 bytes for our minimal response.
    assert_eq!(&dns[dns.len() - 4..], &addr);
}

fn wrap_udp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp = UdpPacketBuilder {
        src_port,
        dst_port,
        payload,
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();
    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0,
        ttl: 64,
        protocol: Ipv4Protocol::UDP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &udp,
    }
    .build_vec()
    .unwrap();
    EthernetFrameBuilder {
        dest_mac: dst_mac,
        src_mac,
        ethertype: EtherType::IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn wrap_tcp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: TcpFlags,
    payload: &[u8],
) -> Vec<u8> {
    let tcp = TcpSegmentBuilder {
        src_port,
        dst_port,
        seq_number: seq,
        ack_number: ack,
        flags,
        window_size: 65535,
        urgent_pointer: 0,
        options: &[],
        payload,
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();
    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0,
        ttl: 64,
        protocol: Ipv4Protocol::TCP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &tcp,
    }
    .build_vec()
    .unwrap();
    EthernetFrameBuilder {
        dest_mac: dst_mac,
        src_mac,
        ethertype: EtherType::IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap()
}

fn build_dhcp_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]);
    out.extend_from_slice(&[53, 1, 1]); // DHCPDISCOVER
    out.push(255);
    out
}

fn build_dhcp_request(
    xid: u32,
    mac: MacAddr,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]);
    out.extend_from_slice(&[53, 1, 3]); // DHCPREQUEST
    out.extend_from_slice(&[50, 4]);
    out.extend_from_slice(&requested_ip.octets());
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&server_id.octets());
    out.push(255);
    out
}

fn build_dns_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}
