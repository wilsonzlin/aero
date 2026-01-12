#![cfg(not(target_arch = "wasm32"))]

use std::net::{Ipv4Addr, SocketAddr};

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

fn assert_dns_has_last_a(frame: &[u8], id: u16, addr: [u8; 4]) {
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    let udp = UdpDatagram::parse(ip.payload()).unwrap();
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

fn build_dns_query(id: u16, name: &str) -> Vec<u8> {
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
    out.extend_from_slice(&(DnsType::A as u16).to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // IN
    out
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

async fn run_case(max_cache_entries: u32, expected_queries_total: u64) {
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
    let _allowed_domains = EnvVarGuard::unset("AERO_L2_ALLOWED_DOMAINS");
    let _blocked_domains = EnvVarGuard::unset("AERO_L2_BLOCKED_DOMAINS");

    let _stack_cache_entries = EnvVarGuard::set(
        "AERO_L2_STACK_MAX_DNS_CACHE_ENTRIES",
        &max_cache_entries.to_string(),
    );
    let _dns_a = EnvVarGuard::set(
        "AERO_L2_DNS_A",
        "first.local=203.0.113.10,second.local=203.0.113.11",
    );

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);

    // Resolve first.local.
    let id1 = 0x1111;
    let query = build_dns_query(id1, "first.local");
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
    let resp = wait_for_dns_response(&mut ws_rx, id1).await;
    assert_dns_has_last_a(&resp, id1, [203, 0, 113, 10]);

    // Resolve second.local.
    let id2 = 0x2222;
    let query = build_dns_query(id2, "second.local");
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
    let resp = wait_for_dns_response(&mut ws_rx, id2).await;
    assert_dns_has_last_a(&resp, id2, [203, 0, 113, 11]);

    // Re-resolve first.local to check cache behavior.
    let id3 = 0x3333;
    let query = build_dns_query(id3, "first.local");
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        guest_ip,
        gateway_ip,
        53002,
        53,
        &query,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&frame).into()))
        .await
        .unwrap();
    let resp = wait_for_dns_response(&mut ws_rx, id3).await;
    assert_dns_has_last_a(&resp, id3, [203, 0, 113, 10]);

    ws_tx.send(Message::Close(None)).await.unwrap();

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let queries_total = parse_metric(&body, "l2_dns_queries_total").unwrap();
    assert_eq!(
        queries_total, expected_queries_total,
        "unexpected proxy DNS query count (cache entries={max_cache_entries})"
    );

    proxy.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stack_dns_cache_entries_env_controls_eviction() {
    let _lock = ENV_LOCK.lock().await;

    // With capacity=1, resolving two unique names should evict the first, forcing a third proxy
    // resolution.
    run_case(1, 3).await;

    // With capacity=2, the first name should still be cached, so the third lookup should be a hit.
    run_case(2, 2).await;
}
