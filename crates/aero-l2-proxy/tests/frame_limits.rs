#![cfg(not(target_arch = "wasm32"))]

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use aero_net_stack::packet::*;
use aero_net_stack::{Action, NetworkStack, StackConfig};
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

fn build_dhcp_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie
    out.extend_from_slice(&[53, 1, 1]); // DHCPDISCOVER
    out.push(255);
    out
}

fn wrap_udp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: core::net::Ipv4Addr,
    dst_ip: core::net::Ipv4Addr,
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

fn max_dhcp_offer_frame_len(discover_frame: &[u8]) -> usize {
    let mut stack = NetworkStack::new(StackConfig::default());
    let actions = stack.process_outbound_ethernet(discover_frame, 0);
    actions
        .into_iter()
        .filter_map(|action| match action {
            Action::EmitFrame(frame) => Some(frame.len()),
            _ => None,
        })
        .max()
        .expect("expected DHCP discover to yield at least one reply frame")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_stack_frames_are_dropped_and_increment_metric() {
    let _lock = ENV_LOCK.lock().await;

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let discover = build_dhcp_discover(0x1020_3040, guest_mac);
    let discover_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        core::net::Ipv4Addr::UNSPECIFIED,
        core::net::Ipv4Addr::BROADCAST,
        68,
        67,
        &discover,
    );
    let discover_len = discover_frame.len();
    let offer_len = max_dhcp_offer_frame_len(&discover_frame);
    assert!(
        offer_len > discover_len,
        "expected DHCP offer to be larger than discover (discover={discover_len}, offer={offer_len})"
    );

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
    let _token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _jwt_secret = EnvVarGuard::unset("AERO_L2_JWT_SECRET");
    let _jwt_audience = EnvVarGuard::unset("AERO_L2_JWT_AUDIENCE");
    let _jwt_issuer = EnvVarGuard::unset("AERO_L2_JWT_ISSUER");
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");
    let _session_secret_alias = EnvVarGuard::unset("SESSION_SECRET");
    let _gateway_session_secret = EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET");
    let _max_connections = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS", "0");
    let _max_connections_per_session = EnvVarGuard::set("AERO_L2_MAX_CONNECTIONS_PER_SESSION", "0");
    let _max_bytes = EnvVarGuard::set("AERO_L2_MAX_BYTES_PER_CONNECTION", "0");
    let _max_fps = EnvVarGuard::set("AERO_L2_MAX_FRAMES_PER_SECOND", "0");
    let _ping_interval = EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0");

    // Tighten the max frame payload so the DHCP OFFER/ACK frames emitted by the stack are too
    // large to be encoded onto the wire, while still accepting the smaller DHCPDISCOVER frame.
    let _max_frame_payload =
        EnvVarGuard::set("AERO_L2_MAX_FRAME_PAYLOAD", &discover_len.to_string());

    let cfg = ProxyConfig::from_env().unwrap();
    assert_eq!(cfg.l2_max_frame_payload, discover_len);

    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline_body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let baseline = parse_metric(&baseline_body, "l2_frames_dropped_total").unwrap_or(0);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, _ws_rx) = ws.split();

    ws_tx
        .send(Message::Binary(encode_l2_frame(&discover_frame).into()))
        .await
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let body = reqwest::get(format!("http://{addr}/metrics"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let dropped = parse_metric(&body, "l2_frames_dropped_total").unwrap_or(0);
        if dropped > baseline {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "expected l2_frames_dropped_total to increment (baseline={baseline}, current={dropped}, offer_len={offer_len})"
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let _ = ws_tx.send(Message::Close(None)).await;
    proxy.shutdown().await;
}
