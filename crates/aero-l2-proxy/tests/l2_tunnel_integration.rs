#![cfg(not(target_arch = "wasm32"))]

use std::net::{Ipv4Addr, SocketAddr};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use aero_net_stack::packet::*;
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::AsyncReadExt,
    io::AsyncWriteExt,
    net::{TcpListener, UdpSocket},
    sync::oneshot,
};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dhcp_arp_dns_tcp_echo_over_l2_tunnel() {
    let echo = start_tcp_echo_server().await;
    let udp_echo = start_udp_echo_server().await;

    let tcp_allowed_port = echo.addr.port();
    let tcp_denied_port = if tcp_allowed_port == u16::MAX {
        u16::MAX - 1
    } else {
        tcp_allowed_port + 1
    };

    let udp_allowed_port = udp_echo.addr.port();
    let udp_denied_port = if udp_allowed_port == u16::MAX {
        u16::MAX - 1
    } else {
        udp_allowed_port + 1
    };

    // Ensure local developer env vars don't accidentally harden the proxy in tests.
    std::env::remove_var("AERO_L2_AUTH_MODE");
    std::env::remove_var("AERO_L2_API_KEY");
    std::env::remove_var("AERO_L2_TOKEN");
    std::env::remove_var("AERO_L2_SESSION_SECRET");
    std::env::remove_var("SESSION_SECRET");
    std::env::remove_var("AERO_L2_JWT_SECRET");
    std::env::remove_var("AERO_L2_MAX_CONNECTIONS_PER_SESSION");

    std::env::set_var("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    // Ensure security/keepalive knobs are deterministic for this end-to-end probe test (do not
    // inherit developer environment overrides).
    std::env::set_var("AERO_L2_OPEN", "0");
    std::env::remove_var("ALLOWED_ORIGINS");
    std::env::remove_var("AERO_L2_AUTH_MODE");
    std::env::remove_var("SESSION_SECRET");
    std::env::remove_var("AERO_L2_SESSION_SECRET");
    std::env::remove_var("AERO_L2_API_KEY");
    std::env::remove_var("AERO_L2_JWT_SECRET");
    std::env::remove_var("AERO_L2_JWT_AUDIENCE");
    std::env::remove_var("AERO_L2_JWT_ISSUER");
    std::env::remove_var("AERO_L2_TOKEN");
    std::env::remove_var("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    std::env::remove_var("AERO_L2_ALLOWED_HOSTS");
    std::env::remove_var("AERO_L2_TRUST_PROXY_HOST");
    std::env::set_var("AERO_L2_MAX_CONNECTIONS", "0");
    std::env::set_var("AERO_L2_MAX_CONNECTIONS_PER_SESSION", "0");
    std::env::set_var("AERO_L2_MAX_BYTES_PER_CONNECTION", "0");
    std::env::set_var("AERO_L2_MAX_FRAMES_PER_SECOND", "0");
    std::env::set_var("AERO_L2_PING_INTERVAL_MS", "0");
    std::env::set_var("AERO_L2_ALLOWED_ORIGINS", "*");
    std::env::set_var("AERO_L2_AUTH_MODE", "none");
    std::env::set_var("AERO_L2_DNS_A", "echo.local=203.0.113.10");
    std::env::set_var("AERO_L2_ALLOWED_TCP_PORTS", tcp_allowed_port.to_string());
    std::env::set_var("AERO_L2_ALLOWED_UDP_PORTS", udp_allowed_port.to_string());
    std::env::set_var(
        "AERO_L2_TCP_FORWARD",
        format!(
            "203.0.113.10:{tcp_allowed_port}=127.0.0.1:{tcp_allowed_port},203.0.113.10:{tcp_denied_port}=127.0.0.1:{tcp_allowed_port}",
        ),
    );
    std::env::set_var(
        "AERO_L2_UDP_FORWARD",
        format!(
            "203.0.113.11:{udp_allowed_port}=127.0.0.1:{udp_allowed_port},203.0.113.11:{udp_denied_port}=127.0.0.1:{udp_allowed_port}",
        ),
    );

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let proxy_addr = proxy.local_addr();

    let ws_url = format!("ws://{proxy_addr}/l2");
    let mut req = ws_url.into_client_request().unwrap();
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(TUNNEL_SUBPROTOCOL),
    );
    req.headers_mut()
        .insert("Origin", HeaderValue::from_static("https://example.test"));
    let (ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);

    // --- DHCP handshake ---
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
    ws_tx
        .send(Message::Binary(encode_l2_frame(&discover_frame).into()))
        .await
        .unwrap();

    // Expect two OFFER frames (broadcast + unicast).
    for _ in 0..2 {
        let frame = wait_for_eth_frame(&mut ws_rx, |f| is_dhcp_type(f, DhcpMessageType::Offer))
            .await
            .unwrap();
        let msg = parse_dhcp_from_frame(&frame).unwrap();
        assert_eq!(msg.message_type, DhcpMessageType::Offer);
    }

    let request = build_dhcp_request(xid, guest_mac, guest_ip, gateway_ip);
    let request_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&request_frame).into()))
        .await
        .unwrap();

    for _ in 0..2 {
        let frame = wait_for_eth_frame(&mut ws_rx, |f| is_dhcp_type(f, DhcpMessageType::Ack))
            .await
            .unwrap();
        let msg = parse_dhcp_from_frame(&frame).unwrap();
        assert_eq!(msg.message_type, DhcpMessageType::Ack);
    }

    // --- ARP for gateway ---
    let arp_req = ArpPacketBuilder {
        opcode: ARP_OP_REQUEST,
        sender_mac: guest_mac,
        sender_ip: guest_ip,
        target_mac: MacAddr([0, 0, 0, 0, 0, 0]),
        target_ip: gateway_ip,
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
    ws_tx
        .send(Message::Binary(encode_l2_frame(&arp_frame).into()))
        .await
        .unwrap();

    let arp_reply = wait_for_eth_frame(&mut ws_rx, |f| {
        let Ok(eth) = EthernetFrame::parse(f) else {
            return false;
        };
        if eth.ethertype() != EtherType::ARP {
            return false;
        }
        let Ok(arp) = ArpPacket::parse(eth.payload()) else {
            return false;
        };
        arp.opcode() == ARP_OP_REPLY && arp.sender_ip() == Some(gateway_ip)
    })
    .await
    .unwrap();
    let eth = EthernetFrame::parse(&arp_reply).unwrap();
    let arp = ArpPacket::parse(eth.payload()).unwrap();
    let gateway_mac = arp.sender_mac().expect("ARP sender MAC");

    // --- Policy sanity check: private IPs are denied by default ---
    let denied_ip = Ipv4Addr::new(10, 0, 0, 1);
    let denied_guest_port = 40000;
    let denied_isn = 1234;
    let denied_syn = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        denied_ip,
        denied_guest_port,
        80,
        denied_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&denied_syn).into()))
        .await
        .unwrap();
    let rst = wait_for_eth_frame(&mut ws_rx, |f| {
        let Ok(seg) = parse_tcp_from_frame(f) else {
            return false;
        };
        seg.src_port() == 80
            && seg.dst_port() == denied_guest_port
            && seg.flags().contains(TcpFlags::RST | TcpFlags::ACK)
            && seg.ack_number() == denied_isn + 1
    })
    .await
    .unwrap();
    let rst_seg = parse_tcp_from_frame(&rst).unwrap();
    assert_eq!(rst_seg.ack_number(), denied_isn + 1);
    // --- Policy sanity check: port allowlist is enforced even with forward-map overrides ---
    let denied_port_guest_port = 40002;
    let denied_port_isn = 2345;
    let denied_port_syn = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        Ipv4Addr::new(203, 0, 113, 10),
        denied_port_guest_port,
        tcp_denied_port,
        denied_port_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&denied_port_syn).into()))
        .await
        .unwrap();
    let denied_port_rst = wait_for_eth_frame(&mut ws_rx, |f| {
        let Ok(seg) = parse_tcp_from_frame(f) else {
            return false;
        };
        seg.src_port() == tcp_denied_port
            && seg.dst_port() == denied_port_guest_port
            && seg.flags().contains(TcpFlags::RST)
            && seg.ack_number() == denied_port_isn + 1
    })
    .await
    .unwrap();
    let denied_port_rst_seg = parse_tcp_from_frame(&denied_port_rst).unwrap();
    assert_eq!(denied_port_rst_seg.ack_number(), denied_port_isn + 1);

    // --- UDP echo probe ---
    let udp_remote_ip = Ipv4Addr::new(203, 0, 113, 11);
    let udp_remote_port = udp_echo.addr.port();
    let udp_guest_port = 50000;
    let udp_payload = b"hi-udp";
    let udp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        udp_remote_ip,
        udp_guest_port,
        udp_remote_port,
        udp_payload,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&udp_frame).into()))
        .await
        .unwrap();

    let udp_resp = wait_for_eth_frame(&mut ws_rx, |f| {
        let Ok(udp) = parse_udp_from_frame(f) else {
            return false;
        };
        udp.src_port() == udp_remote_port
            && udp.dst_port() == udp_guest_port
            && udp.payload() == udp_payload
    })
    .await
    .unwrap();
    let udp = parse_udp_from_frame(&udp_resp).unwrap();
    assert_eq!(udp.payload(), udp_payload);

    // UDP port allowlist should apply even with forward-map overrides.
    let denied_udp_guest_port = 50001;
    let denied_udp_payload = b"hi-udp-denied";
    let denied_udp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        udp_remote_ip,
        denied_udp_guest_port,
        udp_denied_port,
        denied_udp_payload,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&denied_udp_frame).into()))
        .await
        .unwrap();
    let denied_udp_wait = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        wait_for_eth_frame(&mut ws_rx, |f| {
            let Ok(udp) = parse_udp_from_frame(f) else {
                return false;
            };
            udp.src_port() == udp_denied_port
                && udp.dst_port() == denied_udp_guest_port
                && udp.payload() == denied_udp_payload
        })
        .await
    })
    .await;
    assert!(
        denied_udp_wait.is_err(),
        "unexpected UDP response on denied port"
    );

    // --- DNS query for echo.local ---
    let dns_id = 0x1234;
    let dns_query = build_dns_query(dns_id, "echo.local", DnsType::A as u16);
    let dns_frame = wrap_udp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        gateway_ip,
        53000,
        53,
        &dns_query,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dns_frame).into()))
        .await
        .unwrap();

    let dns_resp = wait_for_eth_frame(&mut ws_rx, |f| dns_response_has_a_record(f, dns_id))
        .await
        .unwrap();
    assert_dns_response_has_a_record(&dns_resp, dns_id, [203, 0, 113, 10]);

    // --- TCP echo probe ---
    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let guest_port = 40001;
    let guest_isn = 5000;
    let remote_port = echo.addr.port();

    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn).into()))
        .await
        .unwrap();

    let syn_ack_frame = wait_for_eth_frame(&mut ws_rx, |f| {
        let Ok(seg) = parse_tcp_from_frame(f) else {
            return false;
        };
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.flags().contains(TcpFlags::SYN | TcpFlags::ACK)
            && seg.ack_number() == guest_isn + 1
    })
    .await
    .unwrap();
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame).unwrap();

    let mut next_client_seq = guest_isn + 1;
    let mut next_server_seq = syn_ack.seq_number() + 1;

    // ACK SYN-ACK.
    let ack = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        next_client_seq,
        next_server_seq,
        TcpFlags::ACK,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&ack).into()))
        .await
        .unwrap();

    // Send payload.
    let payload = b"hello";
    let psh = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        next_client_seq,
        next_server_seq,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&psh).into()))
        .await
        .unwrap();
    next_client_seq += payload.len() as u32;

    // Wait for echoed payload.
    let echo_frame = wait_for_eth_frame(&mut ws_rx, |f| {
        let Ok(seg) = parse_tcp_from_frame(f) else {
            return false;
        };
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.seq_number() == next_server_seq
            && !seg.payload().is_empty()
    })
    .await
    .unwrap();
    let echo_seg = parse_tcp_from_frame(&echo_frame).unwrap();
    assert_eq!(echo_seg.payload(), payload);
    next_server_seq += payload.len() as u32;

    // ACK echoed data.
    let ack_data = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        next_client_seq,
        next_server_seq,
        TcpFlags::ACK,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&ack_data).into()))
        .await
        .unwrap();

    // FIN.
    let fin = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        next_client_seq,
        next_server_seq,
        TcpFlags::ACK | TcpFlags::FIN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&fin).into()))
        .await
        .unwrap();
    next_client_seq += 1;

    // Expect the stack to send a FIN to the guest.
    let fin_from_stack_frame = wait_for_eth_frame(&mut ws_rx, |f| {
        let Ok(seg) = parse_tcp_from_frame(f) else {
            return false;
        };
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.flags().contains(TcpFlags::FIN)
    })
    .await
    .unwrap();
    let fin_from_stack = parse_tcp_from_frame(&fin_from_stack_frame).unwrap();

    // Final ACK for stack FIN; stack should drop state.
    let final_ack = wrap_tcp_ipv4_eth(
        guest_mac,
        gateway_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        next_client_seq,
        fin_from_stack.seq_number() + 1,
        TcpFlags::ACK,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&final_ack).into()))
        .await
        .unwrap();

    ws_tx.send(Message::Close(None)).await.unwrap();

    proxy.shutdown().await;
    udp_echo.shutdown().await;
    echo.shutdown().await;
}

fn encode_l2_frame(payload: &[u8]) -> Vec<u8> {
    aero_l2_protocol::encode_frame(payload).unwrap()
}

struct EchoServer {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl EchoServer {
    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

type UdpEchoServer = EchoServer;

async fn start_tcp_echo_server() -> EchoServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accept = listener.accept() => {
                    let (mut socket, _) = match accept {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 16 * 1024];
                        loop {
                            let n = match socket.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            if socket.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            }
        }
    });

    EchoServer {
        addr,
        shutdown: Some(shutdown_tx),
        task: Some(task),
    }
}

async fn start_udp_echo_server() -> UdpEchoServer {
    let socket = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = socket.local_addr().unwrap();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    let task = tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                recv = socket.recv_from(&mut buf) => {
                    let Ok((n, peer)) = recv else {
                        break;
                    };
                    let _ = socket.send_to(&buf[..n], peer).await;
                }
            }
        }
    });

    EchoServer {
        addr,
        shutdown: Some(shutdown_tx),
        task: Some(task),
    }
}

async fn wait_for_eth_frame<F>(
    ws_rx: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    pred: F,
) -> anyhow::Result<Vec<u8>>
where
    F: Fn(&[u8]) -> bool,
{
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(5));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => return Err(anyhow::anyhow!("timed out waiting for Ethernet frame")),
            msg = ws_rx.next() => {
                let Some(msg) = msg else {
                    return Err(anyhow::anyhow!("ws closed"));
                };
                let msg = msg.map_err(|err| anyhow::anyhow!("ws recv error: {err}"))?;
                let Message::Binary(data) = msg else {
                    continue;
                };
                let Ok(decoded) = aero_l2_protocol::decode_message(&data) else {
                    continue;
                };
                if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_FRAME {
                    continue;
                }
                if pred(decoded.payload) {
                    return Ok(decoded.payload.to_vec());
                }
            }
        }
    }
}

fn is_dhcp_type(frame: &[u8], ty: DhcpMessageType) -> bool {
    parse_dhcp_from_frame(frame)
        .map(|m| m.message_type == ty)
        .unwrap_or(false)
}

fn parse_dhcp_from_frame(frame: &[u8]) -> anyhow::Result<DhcpMessage> {
    let eth =
        EthernetFrame::parse(frame).map_err(|err| anyhow::anyhow!("ethernet parse: {err:?}"))?;
    if eth.ethertype() != EtherType::IPV4 {
        return Err(anyhow::anyhow!("not ipv4"));
    }
    let ip =
        Ipv4Packet::parse(eth.payload()).map_err(|err| anyhow::anyhow!("ipv4 parse: {err:?}"))?;
    if ip.protocol() != Ipv4Protocol::UDP {
        return Err(anyhow::anyhow!("not udp"));
    }
    let udp =
        UdpDatagram::parse(ip.payload()).map_err(|err| anyhow::anyhow!("udp parse: {err:?}"))?;
    if udp.src_port() != 67 || udp.dst_port() != 68 {
        return Err(anyhow::anyhow!("not dhcp"));
    }
    DhcpMessage::parse(udp.payload()).map_err(|err| anyhow::anyhow!("dhcp parse: {err:?}"))
}

fn parse_tcp_from_frame(frame: &[u8]) -> anyhow::Result<TcpSegment<'_>> {
    let eth =
        EthernetFrame::parse(frame).map_err(|err| anyhow::anyhow!("ethernet parse: {err:?}"))?;
    if eth.ethertype() != EtherType::IPV4 {
        return Err(anyhow::anyhow!("not ipv4"));
    }
    let ip =
        Ipv4Packet::parse(eth.payload()).map_err(|err| anyhow::anyhow!("ipv4 parse: {err:?}"))?;
    if ip.protocol() != Ipv4Protocol::TCP {
        return Err(anyhow::anyhow!("not tcp"));
    }
    TcpSegment::parse(ip.payload()).map_err(|err| anyhow::anyhow!("tcp parse: {err:?}"))
}

fn parse_udp_from_frame(frame: &[u8]) -> anyhow::Result<UdpDatagram<'_>> {
    let eth =
        EthernetFrame::parse(frame).map_err(|err| anyhow::anyhow!("ethernet parse: {err:?}"))?;
    if eth.ethertype() != EtherType::IPV4 {
        return Err(anyhow::anyhow!("not ipv4"));
    }
    let ip =
        Ipv4Packet::parse(eth.payload()).map_err(|err| anyhow::anyhow!("ipv4 parse: {err:?}"))?;
    if ip.protocol() != Ipv4Protocol::UDP {
        return Err(anyhow::anyhow!("not udp"));
    }
    UdpDatagram::parse(ip.payload()).map_err(|err| anyhow::anyhow!("udp parse: {err:?}"))
}

fn dns_response_has_a_record(frame: &[u8], id: u16) -> bool {
    let Ok(eth) = EthernetFrame::parse(frame) else {
        return false;
    };
    if eth.ethertype() != EtherType::IPV4 {
        return false;
    }
    let Ok(ip) = Ipv4Packet::parse(eth.payload()) else {
        return false;
    };
    if ip.protocol() != Ipv4Protocol::UDP {
        return false;
    }
    let Ok(udp) = UdpDatagram::parse(ip.payload()) else {
        return false;
    };
    if udp.src_port() != 53 {
        return false;
    }
    let dns = udp.payload();
    dns.len() >= 12 && dns[0..2] == id.to_be_bytes()
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
    .expect("build UDP");
    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0x4000, // DF
        ttl: 64,
        protocol: Ipv4Protocol::UDP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &udp,
    }
    .build_vec()
    .expect("build IPv4");
    EthernetFrameBuilder {
        dest_mac: dst_mac,
        src_mac,
        ethertype: EtherType::IPV4,
        payload: &ip,
    }
    .build_vec()
    .expect("build Ethernet frame")
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
    .expect("build TCP");
    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0x4000, // DF
        ttl: 64,
        protocol: Ipv4Protocol::TCP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &tcp,
    }
    .build_vec()
    .expect("build IPv4");
    EthernetFrameBuilder {
        dest_mac: dst_mac,
        src_mac,
        ethertype: EtherType::IPV4,
        payload: &ip,
    }
    .build_vec()
    .expect("build Ethernet frame")
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
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // RD=1
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in name.trim_end_matches('.').split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // IN
    out
}
