#![cfg(not(target_arch = "wasm32"))]

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use aero_net_stack::packet::*;
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
    tokio::time::timeout(Duration::from_secs(2), async {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_send_fail_metric_increments_on_icmp_unreachable() {
    let _lock = ENV_LOCK.lock().await;

    let remote_ip = Ipv4Addr::new(203, 0, 113, 11);
    let remote_port = 9999;

    // Forward to localhost port 1 (almost always closed), which should generate an ICMP port
    // unreachable. On Linux, a connected UDP socket will report `ECONNREFUSED` on a subsequent send.
    let udp_forward = format!("{remote_ip}:{remote_port}=127.0.0.1:1");

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
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");
    let _session_secret_alias = EnvVarGuard::unset("SESSION_SECRET");
    let _gateway_session_secret = EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET");
    let _legacy_token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _allowed_udp = EnvVarGuard::set("AERO_L2_ALLOWED_UDP_PORTS", &remote_port.to_string());
    let _udp_forward = EnvVarGuard::set("AERO_L2_UDP_FORWARD", &udp_forward);
    let _idle = EnvVarGuard::set("AERO_L2_UDP_FLOW_IDLE_TIMEOUT_MS", "0");
    let _ping_interval = EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let start = parse_metric(&baseline, "l2_udp_send_fail_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Mark the guest IP as assigned so the stack emits UDP proxy actions.
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

    let guest_port = 50_000;
    let payload = b"hi".to_vec();
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        &payload,
    );

    // Send multiple datagrams to increase the chance we observe the socket error state on send.
    for _ in 0..3 {
        ws_tx
            .send(Message::Binary(encode_l2_frame(&frame).into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let val = parse_metric(&body, "l2_udp_send_fail_total").unwrap_or(0);
            if val >= start.saturating_add(1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_flows_active_decrements_after_send_error() {
    let _lock = ENV_LOCK.lock().await;

    let remote_ip = Ipv4Addr::new(203, 0, 113, 11);
    let remote_port = 9998;

    let udp_forward = format!("{remote_ip}:{remote_port}=127.0.0.1:1");

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
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");
    let _session_secret_alias = EnvVarGuard::unset("SESSION_SECRET");
    let _gateway_session_secret = EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET");
    let _legacy_token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _allowed_udp = EnvVarGuard::set("AERO_L2_ALLOWED_UDP_PORTS", &remote_port.to_string());
    let _udp_forward = EnvVarGuard::set("AERO_L2_UDP_FORWARD", &udp_forward);
    let _idle = EnvVarGuard::set("AERO_L2_UDP_FLOW_IDLE_TIMEOUT_MS", "0");
    let _ping_interval = EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let send_fail_start = parse_metric(&baseline, "l2_udp_send_fail_total").unwrap_or(0);
    assert_eq!(
        parse_metric(&baseline, "l2_udp_flows_active").unwrap(),
        0,
        "expected no UDP flows at startup"
    );

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

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

    let payload = b"hi".to_vec();
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        50_000,
        remote_port,
        &payload,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&frame).into()))
        .await
        .unwrap();

    // First wait for the flow to be opened.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let flows = parse_metric(&body, "l2_udp_flows_active").unwrap();
            if flows >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    // The connected UDP socket should eventually report `ECONNREFUSED` on a subsequent send. When
    // that happens, the proxy should tear down the flow even when idle timeouts are disabled.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            ws_tx
                .send(Message::Binary(encode_l2_frame(&frame).into()))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;

            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let send_fails = parse_metric(&body, "l2_udp_send_fail_total").unwrap_or(0);
            if send_fails >= send_fail_start.saturating_add(1) {
                break;
            }
        }
    })
    .await
    .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = reqwest::get(format!("http://{addr}/metrics"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let flows = parse_metric(&body, "l2_udp_flows_active").unwrap();
            if flows == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}
