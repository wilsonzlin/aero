#![cfg(not(target_arch = "wasm32"))]

use std::net::{Ipv4Addr, SocketAddr};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use aero_net_stack::packet::*;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;
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

fn encode_l2_frame(payload: &[u8]) -> Vec<u8> {
    aero_l2_protocol::encode_frame(payload).unwrap()
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

async fn wait_for_udp_datagram(
    ws_rx: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    pred: impl Fn(&UdpDatagram<'_>) -> bool,
) -> Vec<u8> {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let Some(msg) = ws_rx.next().await else {
                panic!("ws closed");
            };
            let msg = msg.expect("ws recv");
            let Message::Binary(data) = msg else {
                continue;
            };
            let decoded = match aero_l2_protocol::decode_message(&data) {
                Ok(decoded) => decoded,
                Err(_) => continue,
            };
            if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_FRAME {
                continue;
            }
            let Ok(eth) = EthernetFrame::parse(decoded.payload) else {
                continue;
            };
            if eth.ethertype() != EtherType::IPV4 {
                continue;
            }
            let Ok(ip) = Ipv4Packet::parse(eth.payload()) else {
                continue;
            };
            if ip.protocol() != Ipv4Protocol::UDP {
                continue;
            }
            let Ok(udp) = UdpDatagram::parse(ip.payload()) else {
                continue;
            };
            if pred(&udp) {
                return udp.payload().to_vec();
            }
        }
    })
    .await
    .unwrap()
}

async fn wait_for_tcp_segment(
    ws_rx: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    pred: impl Fn(&TcpSegment<'_>) -> bool,
) -> (TcpFlags, u16, u16, u32, u32) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let Some(msg) = ws_rx.next().await else {
                panic!("ws closed");
            };
            let msg = msg.expect("ws recv");
            let Message::Binary(data) = msg else {
                continue;
            };
            let decoded = match aero_l2_protocol::decode_message(&data) {
                Ok(decoded) => decoded,
                Err(_) => continue,
            };
            if decoded.msg_type != aero_l2_protocol::L2_TUNNEL_TYPE_FRAME {
                continue;
            }
            let Ok(eth) = EthernetFrame::parse(decoded.payload) else {
                continue;
            };
            if eth.ethertype() != EtherType::IPV4 {
                continue;
            }
            let Ok(ip) = Ipv4Packet::parse(eth.payload()) else {
                continue;
            };
            if ip.protocol() != Ipv4Protocol::TCP {
                continue;
            }
            let Ok(seg) = TcpSegment::parse(ip.payload()) else {
                continue;
            };
            if pred(&seg) {
                return (
                    seg.flags(),
                    seg.src_port(),
                    seg.dst_port(),
                    seg.seq_number(),
                    seg.ack_number(),
                );
            }
        }
    })
    .await
    .unwrap()
}

fn build_dhcp_request(
    xid: u32,
    mac: MacAddr,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    // Construct a minimal DHCPREQUEST packet. `aero-net-stack` only needs the message type and
    // client MAC to mark the guest IP as assigned.
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie
    out.extend_from_slice(&[53, 1, 3]); // DHCPREQUEST
    out.extend_from_slice(&[50, 4]);
    out.extend_from_slice(&requested_ip.octets());
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&server_id.octets());
    out.push(255);
    out
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

async fn run_buffer_case(max_buffered_tcp_bytes: u32, expect_rst: bool) {
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
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
    let _allow_private_ips = EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS");
    let _allowed_tcp_ports = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "80");
    let _stack_max_tcp = EnvVarGuard::unset("AERO_L2_STACK_MAX_TCP_CONNECTIONS");
    let _stack_max_buffered = EnvVarGuard::set(
        "AERO_L2_STACK_MAX_BUFFERED_TCP_BYTES_PER_CONN",
        &max_buffered_tcp_bytes.to_string(),
    );

    // Ensure the proxy-side connection will not complete quickly (so the stack is forced to
    // buffer payload).
    let _tcp_forward = EnvVarGuard::set("AERO_L2_TCP_FORWARD", "203.0.113.10:80=10.255.255.1:80");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack emits TCP frames.
    let dhcp_request = build_dhcp_request(0x1020_3040, guest_mac, guest_ip, gateway_ip);
    let dhcp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &dhcp_request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dhcp_frame).into()))
        .await
        .unwrap();
    let _ = wait_for_udp_datagram(&mut ws_rx, |udp| {
        udp.src_port() == 67 && udp.dst_port() == 68
    })
    .await;

    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let remote_port = 80;
    let guest_port = 40_000;
    let isn = 1234;
    let payload = b"hello";

    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn).into()))
        .await
        .unwrap();

    let data = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn + 1,
        0,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&data).into()))
        .await
        .unwrap();

    if expect_rst {
        let (_flags, _src_port, _dst_port, _seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
            seg.src_port() == remote_port
                && seg.dst_port() == guest_port
                && seg.flags().contains(TcpFlags::RST)
        })
        .await;
        assert_eq!(
            ack,
            isn + 1,
            "expected buffered payload to be rejected (buffer limit={max_buffered_tcp_bytes})"
        );
    } else {
        let (_flags, _src_port, _dst_port, _seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
            seg.src_port() == remote_port
                && seg.dst_port() == guest_port
                && seg.flags() == TcpFlags::ACK
                && seg.ack_number() == isn + 1 + payload.len() as u32
        })
        .await;
        assert_eq!(
            ack,
            isn + 1 + payload.len() as u32,
            "expected buffered payload to be accepted (buffer limit={max_buffered_tcp_bytes})"
        );
    }

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stack_max_tcp_connections_zero_rejects_syn() {
    let _lock = ENV_LOCK.lock().await;

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
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
    let _allow_private_ips = EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS");
    let _allowed_tcp_ports = EnvVarGuard::unset("AERO_L2_ALLOWED_TCP_PORTS");
    let _stack_max_tcp = EnvVarGuard::set("AERO_L2_STACK_MAX_TCP_CONNECTIONS", "0");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack emits TCP RSTs.
    let dhcp_request = build_dhcp_request(0x1020_3040, guest_mac, guest_ip, gateway_ip);
    let dhcp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &dhcp_request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dhcp_frame).into()))
        .await
        .unwrap();
    let _ = wait_for_udp_datagram(&mut ws_rx, |udp| {
        udp.src_port() == 67 && udp.dst_port() == 68
    })
    .await;

    let remote_ip = Ipv4Addr::new(8, 8, 8, 8);
    let remote_port = 80;
    let guest_port = 40_000;
    let isn = 1234;

    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn).into()))
        .await
        .unwrap();

    let (flags, _src_port, _dst_port, _seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.flags().contains(TcpFlags::RST | TcpFlags::ACK)
            && seg.ack_number() == isn + 1
    })
    .await;
    assert!(flags.contains(TcpFlags::RST));
    assert_eq!(ack, isn + 1);

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stack_max_tcp_connections_env_rejects_new_syn_when_at_capacity() {
    let _lock = ENV_LOCK.lock().await;

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
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
    let _allow_private_ips = EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS");
    let _allowed_tcp_ports = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "80");
    let _stack_max_tcp = EnvVarGuard::set("AERO_L2_STACK_MAX_TCP_CONNECTIONS", "1");
    let _stack_max_buffered = EnvVarGuard::unset("AERO_L2_STACK_MAX_BUFFERED_TCP_BYTES_PER_CONN");

    // Ensure the proxy-side connection will not complete quickly.
    let _tcp_forward = EnvVarGuard::set("AERO_L2_TCP_FORWARD", "203.0.113.10:80=10.255.255.1:80");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack emits TCP RSTs.
    let dhcp_request = build_dhcp_request(0x1020_3040, guest_mac, guest_ip, gateway_ip);
    let dhcp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &dhcp_request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dhcp_frame).into()))
        .await
        .unwrap();
    let _ = wait_for_udp_datagram(&mut ws_rx, |udp| {
        udp.src_port() == 67 && udp.dst_port() == 68
    })
    .await;

    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let remote_port = 80;

    // First connection occupies the only available slot.
    let syn1 = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        40_000,
        remote_port,
        1000,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn1).into()))
        .await
        .unwrap();

    // Second SYN should be rejected (RST) because the stack is at capacity.
    let isn2 = 2000;
    let guest_port2 = 40_001;
    let syn2 = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port2,
        remote_port,
        isn2,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn2).into()))
        .await
        .unwrap();

    let (_flags, _src_port, _dst_port, _seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port2
            && seg.flags().contains(TcpFlags::RST)
            && seg.ack_number() == isn2 + 1
    })
    .await;
    assert_eq!(ack, isn2 + 1);

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stack_max_buffered_tcp_bytes_env_controls_rst_vs_ack() {
    let _lock = ENV_LOCK.lock().await;

    run_buffer_case(0, true).await;
    run_buffer_case(1024, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_connect_failures_increment_metric() {
    let _lock = ENV_LOCK.lock().await;

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
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
    let _allow_private_ips = EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS");
    let _allowed_tcp_ports = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "80");
    let _tcp_forward = EnvVarGuard::set("AERO_L2_TCP_FORWARD", "203.0.113.10:80=127.0.0.1:0");
    let _tcp_timeout = EnvVarGuard::set("AERO_L2_TCP_CONNECT_TIMEOUT_MS", "200");
    let _stack_max_tcp = EnvVarGuard::unset("AERO_L2_STACK_MAX_TCP_CONNECTIONS");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let start = parse_metric(&baseline, "l2_tcp_connect_fail_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack emits TCP RSTs.
    let dhcp_request = build_dhcp_request(0x1020_3040, guest_mac, guest_ip, gateway_ip);
    let dhcp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &dhcp_request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dhcp_frame).into()))
        .await
        .unwrap();
    let _ = wait_for_udp_datagram(&mut ws_rx, |udp| {
        udp.src_port() == 67 && udp.dst_port() == 68
    })
    .await;

    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let remote_port = 80;
    let guest_port = 40_000;
    let isn = 1234;

    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn).into()))
        .await
        .unwrap();

    // Expect a TCP RST after the proxy-side connection fails (Forward mapped to port 0).
    let (flags, _src_port, _dst_port, _seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.flags().contains(TcpFlags::RST)
            && seg.ack_number() == isn + 1
    })
    .await;
    assert!(flags.contains(TcpFlags::RST));
    assert_eq!(ack, isn + 1);

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let val = parse_metric(&body, "l2_tcp_connect_fail_total").unwrap_or(0);
    assert!(
        val >= start.saturating_add(1),
        "expected tcp connect fail counter to increment (before={start}, after={val})"
    );

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_conns_active_gauge_tracks_open_connections() {
    let _lock = ENV_LOCK.lock().await;

    // Host-side listener used for deterministic TCP proxy forwarding.
    let tcp_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let tcp_port = tcp_listener.local_addr().unwrap().port();
    let tcp_forward = format!("203.0.113.10:80=127.0.0.1:{tcp_port}");

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
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
    let _allow_private_ips = EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS");
    let _allowed_tcp_ports = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "80");
    let _tcp_forward = EnvVarGuard::set("AERO_L2_TCP_FORWARD", &tcp_forward);
    let _stack_max_tcp = EnvVarGuard::unset("AERO_L2_STACK_MAX_TCP_CONNECTIONS");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        parse_metric(&baseline, "l2_tcp_conns_active").unwrap(),
        0,
        "expected no TCP conns at startup"
    );

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack accepts TCP connections.
    let dhcp_request = build_dhcp_request(0x1020_3040, guest_mac, guest_ip, gateway_ip);
    let dhcp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &dhcp_request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dhcp_frame).into()))
        .await
        .unwrap();
    let _ = wait_for_udp_datagram(&mut ws_rx, |udp| {
        udp.src_port() == 67 && udp.dst_port() == 68
    })
    .await;

    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let remote_port = 80;
    let guest_port = 40_000;
    let isn = 1234;
    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn).into()))
        .await
        .unwrap();

    let (_flags, _src_port, _dst_port, _seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.flags().contains(TcpFlags::SYN | TcpFlags::ACK)
            && seg.ack_number() == isn + 1
    })
    .await;
    assert_eq!(ack, isn + 1);

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let active = parse_metric(&body, "l2_tcp_conns_active").unwrap();
            if active == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let _ = ws_tx.send(Message::Close(None)).await;

    // Ensure the TCP connection is removed once the session ends.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let active = parse_metric(&body, "l2_tcp_conns_active").unwrap();
            if active == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    drop(tcp_listener);
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_denied_metric_increments_for_denied_tcp_port() {
    let _lock = ENV_LOCK.lock().await;

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
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
    let _allow_private_ips = EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS");
    let _allowed_tcp_ports = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "80");
    let _tcp_forward = EnvVarGuard::unset("AERO_L2_TCP_FORWARD");
    let _tcp_timeout = EnvVarGuard::set("AERO_L2_TCP_CONNECT_TIMEOUT_MS", "200");
    let _stack_max_tcp = EnvVarGuard::unset("AERO_L2_STACK_MAX_TCP_CONNECTIONS");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let start = parse_metric(&baseline, "l2_policy_denied_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack emits TCP frames.
    let dhcp_request = build_dhcp_request(0x1020_3040, guest_mac, guest_ip, gateway_ip);
    let dhcp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &dhcp_request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dhcp_frame).into()))
        .await
        .unwrap();
    let _ = wait_for_udp_datagram(&mut ws_rx, |udp| {
        udp.src_port() == 67 && udp.dst_port() == 68
    })
    .await;

    // Attempt to connect to a TCP port outside the allowlist; the proxy should reject before it
    // creates any outbound socket state, incrementing `l2_policy_denied_total`.
    let remote_ip = Ipv4Addr::new(8, 8, 8, 8);
    let remote_port = 81;
    let guest_port = 40_000;
    let isn = 1234;

    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn).into()))
        .await
        .unwrap();

    let (_flags, _src_port, _dst_port, _seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.flags().contains(TcpFlags::RST)
    })
    .await;
    assert_eq!(ack, isn + 1);

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let val = parse_metric(&body, "l2_policy_denied_total").unwrap_or(0);
            if val >= start.saturating_add(1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_conns_active_gauge_decrements_after_guest_rst() {
    let _lock = ENV_LOCK.lock().await;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let tcp_port = listener.local_addr().unwrap().port();
    let tcp_forward = format!("203.0.113.10:80=127.0.0.1:{tcp_port}");

    // Keep the TCP listener alive until the guest closes the connection.
    let accept_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 1];
        let _ = stream.read(&mut buf).await;
    });

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
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
    let _allow_private_ips = EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS");
    let _allowed_tcp_ports = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "80");
    let _tcp_forward = EnvVarGuard::set("AERO_L2_TCP_FORWARD", &tcp_forward);
    let _stack_max_tcp = EnvVarGuard::unset("AERO_L2_STACK_MAX_TCP_CONNECTIONS");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(parse_metric(&baseline, "l2_tcp_conns_active").unwrap(), 0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack emits TCP frames.
    let dhcp_request = build_dhcp_request(0x1020_3040, guest_mac, guest_ip, gateway_ip);
    let dhcp_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &dhcp_request,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dhcp_frame).into()))
        .await
        .unwrap();
    let _ = wait_for_udp_datagram(&mut ws_rx, |udp| {
        udp.src_port() == 67 && udp.dst_port() == 68
    })
    .await;

    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let remote_port = 80;
    let guest_port = 40_000;
    let isn = 1234;

    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&syn).into()))
        .await
        .unwrap();

    let (_flags, _src_port, _dst_port, server_seq, ack) = wait_for_tcp_segment(&mut ws_rx, |seg| {
        seg.src_port() == remote_port
            && seg.dst_port() == guest_port
            && seg.flags().contains(TcpFlags::SYN | TcpFlags::ACK)
    })
    .await;
    assert_eq!(ack, isn + 1);

    // Wait for the proxy to record the opened TCP connection.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let active = parse_metric(&body, "l2_tcp_conns_active").unwrap();
            if active == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    // Complete the handshake then immediately reset from the guest side.
    let ack_pkt = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn + 1,
        server_seq + 1,
        TcpFlags::ACK,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&ack_pkt).into()))
        .await
        .unwrap();

    let rst_pkt = wrap_tcp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        isn + 1,
        server_seq + 1,
        TcpFlags::RST,
        &[],
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&rst_pkt).into()))
        .await
        .unwrap();

    // The session stays open, so the gauge should drop back to 0 once the proxy processes the close.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let active = parse_metric(&body, "l2_tcp_conns_active").unwrap();
            if active == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
    let _ = accept_task.await;
}
