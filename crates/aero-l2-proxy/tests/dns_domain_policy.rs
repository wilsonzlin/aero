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

async fn wait_for_dns_response(
    ws_rx: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    id: u16,
) -> Vec<u8> {
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(5));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => panic!("timed out waiting for DNS response"),
            msg = ws_rx.next() => {
                let Some(msg) = msg else {
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
                if !is_dns_response(decoded.payload, id) {
                    continue;
                }
                return decoded.payload.to_vec();
            }
        }
    }
}

fn is_dns_response(frame: &[u8], id: u16) -> bool {
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

fn assert_dns_rcode_and_ancount(frame: &[u8], id: u16, expected_rcode: u8, expected_ancount: u16) {
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    let udp = UdpDatagram::parse(ip.payload()).unwrap();
    let dns = udp.payload();
    assert_eq!(&dns[0..2], &id.to_be_bytes());
    // QR=1
    assert_eq!(dns[2] & 0x80, 0x80);
    let rcode = dns[3] & 0x0f;
    assert_eq!(rcode, expected_rcode, "unexpected DNS rcode");
    let ancount = u16::from_be_bytes([dns[6], dns[7]]);
    assert_eq!(ancount, expected_ancount, "unexpected DNS ANCOUNT");
}

fn assert_dns_has_last_a(frame: &[u8], id: u16, addr: [u8; 4]) {
    assert_dns_rcode_and_ancount(frame, id, 0, 1);
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    let udp = UdpDatagram::parse(ip.payload()).unwrap();
    let dns = udp.payload();
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

async fn run_dns_case(
    allowed_domains: Option<&str>,
    blocked_domains: Option<&str>,
    expect_allowed_ok: bool,
    expect_blocked_ok: bool,
) {
    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _insecure_no_auth = EnvVarGuard::set("AERO_L2_INSECURE_ALLOW_NO_AUTH", "1");
    let _allowed_origins = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _allowed_origins_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    // Explicitly disable auth for this integration test so we don't rely on implicit defaults.
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
    let _api_key = EnvVarGuard::unset("AERO_L2_API_KEY");
    let _jwt_secret = EnvVarGuard::unset("AERO_L2_JWT_SECRET");
    let _session_secret = EnvVarGuard::unset("AERO_L2_SESSION_SECRET");
    let _session_secret_alias = EnvVarGuard::unset("SESSION_SECRET");
    let _gateway_session_secret = EnvVarGuard::unset("AERO_GATEWAY_SESSION_SECRET");
    let _legacy_token = EnvVarGuard::unset("AERO_L2_TOKEN");
    let _ping_interval = EnvVarGuard::set("AERO_L2_PING_INTERVAL_MS", "0");

    let _allowed_domains_guard = match allowed_domains {
        Some(v) => EnvVarGuard::set("AERO_L2_ALLOWED_DOMAINS", v),
        None => EnvVarGuard::unset("AERO_L2_ALLOWED_DOMAINS"),
    };
    let _blocked_domains_guard = match blocked_domains {
        Some(v) => EnvVarGuard::set("AERO_L2_BLOCKED_DOMAINS", v),
        None => EnvVarGuard::unset("AERO_L2_BLOCKED_DOMAINS"),
    };

    // Deterministic DNS answers without relying on external resolvers.
    let _dns_a = EnvVarGuard::set(
        "AERO_L2_DNS_A",
        "allowed.local=203.0.113.10,blocked.local=203.0.113.11",
    );

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let baseline = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let policy_denied_start = parse_metric(&baseline, "l2_policy_denied_total").unwrap_or(0);
    let expected_denied = u64::from(!expect_allowed_ok) + u64::from(!expect_blocked_ok);

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);

    // Allowed domain resolves.
    let id_allowed = 0x1111;
    let query = build_dns_query(id_allowed, "allowed.local", DnsType::A as u16);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        guest_ip,
        gateway_ip,
        53000,
        53,
        &query,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&frame).into()))
        .await
        .unwrap();
    let resp = wait_for_dns_response(&mut ws_rx, id_allowed).await;
    if expect_allowed_ok {
        assert_dns_has_last_a(&resp, id_allowed, [203, 0, 113, 10]);
    } else {
        assert_dns_rcode_and_ancount(&resp, id_allowed, 3, 0);
    }

    // Blocked domain should be NXDOMAIN when disallowed by policy.
    let id_blocked = 0x2222;
    let query = build_dns_query(id_blocked, "blocked.local", DnsType::A as u16);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        guest_ip,
        gateway_ip,
        53001,
        53,
        &query,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&frame).into()))
        .await
        .unwrap();
    let resp = wait_for_dns_response(&mut ws_rx, id_blocked).await;
    if expect_blocked_ok {
        assert_dns_has_last_a(&resp, id_blocked, [203, 0, 113, 11]);
    } else {
        assert_dns_rcode_and_ancount(&resp, id_blocked, 3, 0);
    }

    if expected_denied > 0 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let body = reqwest::get(format!("http://{addr}/metrics"))
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap();
                let denied = parse_metric(&body, "l2_policy_denied_total").unwrap_or(0);
                if denied >= policy_denied_start.saturating_add(expected_denied) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    ws_tx.send(Message::Close(None)).await.unwrap();
    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dns_domain_allow_and_block_lists_affect_resolution() {
    let _lock = ENV_LOCK.lock().await;

    // Baseline: no lists => everything resolves.
    run_dns_case(None, None, true, true).await;

    // Allowlist blocks non-matching names.
    run_dns_case(Some("allowed.local"), None, true, false).await;
    // Blocklist blocks matching names while allowing others.
    run_dns_case(None, Some("blocked.local"), true, false).await;
}
