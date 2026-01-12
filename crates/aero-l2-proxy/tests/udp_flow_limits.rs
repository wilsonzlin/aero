#![cfg(not(target_arch = "wasm32"))]

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use aero_l2_proxy::{start_server, ProxyConfig, TUNNEL_SUBPROTOCOL};
use aero_net_stack::packet::*;
use futures_util::{SinkExt, StreamExt};
use tokio::{
    net::UdpSocket,
    sync::{oneshot, Mutex},
};
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

async fn start_udp_echo_server() -> EchoServer {
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

fn base_ws_request(addr: SocketAddr) -> tokio_tungstenite::tungstenite::http::Request<()> {
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

async fn wait_for_udp_frame<F>(
    ws_rx: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    pred: F,
) -> anyhow::Result<Vec<u8>>
where
    F: Fn(&UdpDatagram<'_>) -> bool,
{
    loop {
        let Some(msg) = ws_rx.next().await else {
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
            return Ok(udp.payload().to_vec());
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
async fn udp_flow_cap_drops_new_flows() {
    let _lock = ENV_LOCK.lock().await;

    let udp_echo = start_udp_echo_server().await;
    let udp_port = udp_echo.addr.port();

    let udp_port_str = udp_port.to_string();
    let udp_forward = format!("203.0.113.11:{udp_port}=127.0.0.1:{udp_port}");

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _allowed_udp = EnvVarGuard::set("AERO_L2_ALLOWED_UDP_PORTS", &udp_port_str);
    let _udp_forward = EnvVarGuard::set("AERO_L2_UDP_FORWARD", &udp_forward);
    let _cap = EnvVarGuard::set("AERO_L2_MAX_UDP_FLOWS_PER_TUNNEL", "2");
    let _idle = EnvVarGuard::set("AERO_L2_UDP_FLOW_IDLE_TIMEOUT_MS", "0");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = base_ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let remote_ip = Ipv4Addr::new(203, 0, 113, 11);

    // Mark the guest IP as assigned so the stack emits proxy actions.
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

    // Create two distinct UDP flows and ensure we receive responses.
    for i in 0..2u16 {
        let guest_port = 50_000 + i;
        let payload = format!("hi-{i}").into_bytes();
        let frame = wrap_udp_ipv4_eth(
            guest_mac, stack_mac, guest_ip, remote_ip, guest_port, udp_port, &payload,
        );
        ws_tx
            .send(Message::Binary(encode_l2_frame(&frame).into()))
            .await
            .unwrap();

        let resp = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_udp_frame(&mut ws_rx, |udp| {
                udp.src_port() == udp_port
                    && udp.dst_port() == guest_port
                    && udp.payload() == payload
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resp, payload);
    }

    // Third distinct flow should be dropped (no response) and counted by the metric.
    let dropped_guest_port = 50_002;
    let dropped_payload = b"hi-dropped".to_vec();
    let dropped_frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack_mac,
        guest_ip,
        remote_ip,
        dropped_guest_port,
        udp_port,
        &dropped_payload,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&dropped_frame).into()))
        .await
        .unwrap();

    let dropped = tokio::time::timeout(
        Duration::from_millis(200),
        wait_for_udp_frame(&mut ws_rx, |udp| {
            udp.src_port() == udp_port
                && udp.dst_port() == dropped_guest_port
                && udp.payload() == dropped_payload
        }),
    )
    .await;
    assert!(dropped.is_err(), "unexpected response for capped UDP flow");

    let body = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let flows = parse_metric(&body, "l2_udp_flows_active").unwrap();
    assert_eq!(flows, 2);
    let exceeded = parse_metric(&body, "l2_udp_flow_limit_exceeded_total").unwrap();
    assert_eq!(exceeded, 1);

    let _ = ws_tx.send(Message::Close(None)).await;

    proxy.shutdown().await;
    udp_echo.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_flow_idle_timeout_closes_flow() {
    let _lock = ENV_LOCK.lock().await;

    let udp_echo = start_udp_echo_server().await;
    let udp_port = udp_echo.addr.port();

    let udp_port_str = udp_port.to_string();
    let udp_forward = format!("203.0.113.11:{udp_port}=127.0.0.1:{udp_port}");

    let _listen = EnvVarGuard::set("AERO_L2_PROXY_LISTEN_ADDR", "127.0.0.1:0");
    let _open = EnvVarGuard::set("AERO_L2_OPEN", "1");
    let _auth_mode = EnvVarGuard::set("AERO_L2_AUTH_MODE", "none");
    let _allowed = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS");
    let _fallback_allowed = EnvVarGuard::unset("ALLOWED_ORIGINS");
    let _allowed_extra = EnvVarGuard::unset("AERO_L2_ALLOWED_ORIGINS_EXTRA");
    let _allowed_hosts = EnvVarGuard::unset("AERO_L2_ALLOWED_HOSTS");
    let _trust_proxy_host = EnvVarGuard::unset("AERO_L2_TRUST_PROXY_HOST");
    let _allowed_udp = EnvVarGuard::set("AERO_L2_ALLOWED_UDP_PORTS", &udp_port_str);
    let _udp_forward = EnvVarGuard::set("AERO_L2_UDP_FORWARD", &udp_forward);
    let _cap = EnvVarGuard::set("AERO_L2_MAX_UDP_FLOWS_PER_TUNNEL", "0");
    let _idle = EnvVarGuard::set("AERO_L2_UDP_FLOW_IDLE_TIMEOUT_MS", "50");

    let cfg = ProxyConfig::from_env().unwrap();
    let proxy = start_server(cfg).await.unwrap();
    let addr = proxy.local_addr();

    let req = base_ws_request(addr);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut ws_tx, mut ws_rx) = ws.split();

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
    let stack_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let remote_ip = Ipv4Addr::new(203, 0, 113, 11);

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

    let guest_port = 50_000;
    let payload = b"hi-idle".to_vec();
    let frame = wrap_udp_ipv4_eth(
        guest_mac, stack_mac, guest_ip, remote_ip, guest_port, udp_port, &payload,
    );
    ws_tx
        .send(Message::Binary(encode_l2_frame(&frame).into()))
        .await
        .unwrap();

    let resp = tokio::time::timeout(
        Duration::from_secs(2),
        wait_for_udp_frame(&mut ws_rx, |udp| {
            udp.src_port() == udp_port && udp.dst_port() == guest_port && udp.payload() == payload
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(resp, payload);

    // Wait for the idle timeout to close and remove the UDP flow.
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

    // The flow should be able to re-open after being cleaned up.
    ws_tx
        .send(Message::Binary(encode_l2_frame(&frame).into()))
        .await
        .unwrap();
    let resp = tokio::time::timeout(
        Duration::from_secs(2),
        wait_for_udp_frame(&mut ws_rx, |udp| {
            udp.src_port() == udp_port && udp.dst_port() == guest_port && udp.payload() == payload
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(resp, payload);

    let _ = ws_tx.send(Message::Close(None)).await;

    proxy.shutdown().await;
    udp_echo.shutdown().await;
}
