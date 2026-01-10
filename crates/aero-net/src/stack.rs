use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::protocol::{
    build_arp_packet, build_dhcp_ack, build_dhcp_offer, build_dns_response_nxdomain,
    build_ethernet_frame, build_ipv4_packet, build_tcp_segment, build_udp_packet, parse_arp_packet,
    parse_dhcp, parse_dns_query, parse_ethernet_frame, parse_ipv4_packet, parse_tcp_segment,
    parse_udp_packet, ArpOp, DhcpMessageType, DnsQuery, MacAddr, DHCP_CLIENT_PORT,
    DHCP_SERVER_PORT, ETHERTYPE_ARP, ETHERTYPE_IPV4, IP_PROTO_TCP, IP_PROTO_UDP, TCP_FLAG_ACK,
    TCP_FLAG_FIN, TCP_FLAG_PSH, TCP_FLAG_RST, TCP_FLAG_SYN,
};

#[derive(Debug, Clone)]
pub struct StackConfig {
    pub router_mac: MacAddr,
    pub router_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub lease_ip: Ipv4Addr,
    pub dns_ip: Ipv4Addr,
    pub proxy_ws_addr: SocketAddr,
    pub doh_http_addr: SocketAddr,
}

#[derive(Debug)]
pub struct StackHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    active_tcp_connections: Arc<AtomicUsize>,
}

impl StackHandle {
    pub fn active_tcp_connections(&self) -> usize {
        self.active_tcp_connections.load(Ordering::SeqCst)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for StackHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TcpConnKey {
    guest_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
}

#[derive(Debug)]
struct TcpConn {
    guest_mac: MacAddr,
    guest_ip: Ipv4Addr,
    state: TcpConnState,
    guest_next_seq: u32,
    stack_next_seq: u32,
    stack_isn: u32,
    outbound_tx: Option<mpsc::Sender<Vec<u8>>>,
}

#[derive(Debug)]
enum TcpConnState {
    Connecting { guest_isn: u32 },
    SynAckSent { guest_isn: u32 },
    Established,
    Closing,
}

#[derive(Debug)]
enum RemoteEvent {
    Connected { key: TcpConnKey },
    Data { key: TcpConnKey, data: Vec<u8> },
    Closed { key: TcpConnKey },
    Failed { key: TcpConnKey, error: String },
}

pub fn spawn_stack(
    cfg: StackConfig,
    mut from_guest: mpsc::Receiver<Vec<u8>>,
    to_guest: mpsc::Sender<Vec<u8>>,
) -> StackHandle {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let active_tcp_connections = Arc::new(AtomicUsize::new(0));
    let active_tcp_connections_task = active_tcp_connections.clone();

    let task = tokio::spawn(async move {
        let (remote_event_tx, mut remote_event_rx) = mpsc::channel::<RemoteEvent>(256);

        let mut guest_mac: Option<MacAddr> = None;

        let mut tcp_conns: HashMap<TcpConnKey, TcpConn> = HashMap::new();

        info!(router_ip=%cfg.router_ip, "net stack started");

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    info!("net stack shutdown requested");
                    break;
                }
                ev = remote_event_rx.recv() => {
                    let Some(ev) = ev else { break };
                    match ev {
                        RemoteEvent::Connected { key } => {
                            if let Some(conn) = tcp_conns.get_mut(&key) {
                                let TcpConnState::Connecting { guest_isn } = conn.state else { continue };
                                conn.state = TcpConnState::SynAckSent { guest_isn };
                                send_tcp_segment_to_guest(
                                    &cfg,
                                    &to_guest,
                                    conn.guest_mac,
                                    conn.guest_ip,
                                    key.remote_ip,
                                    key.remote_port,
                                    key.guest_port,
                                    conn.stack_isn,
                                    guest_isn.wrapping_add(1),
                                    TCP_FLAG_SYN | TCP_FLAG_ACK,
                                    &[],
                                )
                                .await;
                                debug!(?key, "sent SYN-ACK to guest");
                            }
                        }
                        RemoteEvent::Data { key, data } => {
                            if let Some(conn) = tcp_conns.get_mut(&key) {
                                match conn.state {
                                    TcpConnState::Established | TcpConnState::Closing => {
                                        let seq = conn.stack_next_seq;
                                        let ack = conn.guest_next_seq;
                                        send_tcp_segment_to_guest(
                                            &cfg,
                                            &to_guest,
                                            conn.guest_mac,
                                            conn.guest_ip,
                                            key.remote_ip,
                                            key.remote_port,
                                            key.guest_port,
                                            seq,
                                            ack,
                                            TCP_FLAG_ACK | TCP_FLAG_PSH,
                                            &data,
                                        )
                                        .await;
                                        conn.stack_next_seq = conn.stack_next_seq.wrapping_add(data.len() as u32);
                                        debug!(?key, len=data.len(), "forwarded remote -> guest");
                                    }
                                    _ => {}
                                }
                            }
                        }
                        RemoteEvent::Closed { key } => {
                            if let Some(conn) = tcp_conns.get_mut(&key) {
                                if matches!(conn.state, TcpConnState::Established) {
                                    // Passive close from remote.
                                    let seq = conn.stack_next_seq;
                                    let ack = conn.guest_next_seq;
                                    send_tcp_segment_to_guest(
                                        &cfg,
                                        &to_guest,
                                        conn.guest_mac,
                                        conn.guest_ip,
                                        key.remote_ip,
                                        key.remote_port,
                                        key.guest_port,
                                        seq,
                                        ack,
                                        TCP_FLAG_FIN | TCP_FLAG_ACK,
                                        &[],
                                    )
                                    .await;
                                    conn.stack_next_seq = conn.stack_next_seq.wrapping_add(1);
                                    conn.state = TcpConnState::Closing;
                                }
                            }
                        }
                        RemoteEvent::Failed { key, error } => {
                            warn!(?key, %error, "remote connect failed");
                            if let Some(conn) = tcp_conns.remove(&key) {
                                active_tcp_connections_task.fetch_sub(1, Ordering::SeqCst);
                                send_tcp_segment_to_guest(
                                    &cfg,
                                    &to_guest,
                                    conn.guest_mac,
                                    conn.guest_ip,
                                    key.remote_ip,
                                    key.remote_port,
                                    key.guest_port,
                                    0,
                                    conn.guest_next_seq,
                                    TCP_FLAG_RST | TCP_FLAG_ACK,
                                    &[],
                                )
                                .await;
                            }
                        }
                    }
                }
                frame = from_guest.recv() => {
                    let Some(frame) = frame else { break };
                    let Some(eth) = parse_ethernet_frame(&frame) else { continue };
                    guest_mac.get_or_insert(eth.src);
                    match eth.ethertype {
                        ETHERTYPE_ARP => {
                            handle_arp(&cfg, &to_guest, eth.src, eth.payload).await;
                        }
                        ETHERTYPE_IPV4 => {
                            let Some(ip) = parse_ipv4_packet(eth.payload) else { continue };
                            match ip.protocol {
                                IP_PROTO_UDP => {
                                    let Some(udp) = parse_udp_packet(ip.src, ip.dst, ip.payload) else { continue };
                                    if udp.dst_port == DHCP_SERVER_PORT && udp.src_port == DHCP_CLIENT_PORT {
                                        if let Some(parsed) = parse_dhcp(udp.payload) {
                                            handle_dhcp(&cfg, &to_guest, udp.src_port, udp.dst_port, parsed).await;
                                        }
                                    } else if udp.dst_port == 53 {
                                        if let Some(query) = parse_dns_query(udp.payload) {
                                            let guest_mac = eth.src;
                                            let guest_ip = ip.src;
                                            let src_port = udp.src_port;
                                            let raw_query = udp.payload.to_vec();
                                            let tx = to_guest.clone();
                                            let cfg = cfg.clone();
                                            tokio::spawn(async move {
                                                if let Err(err) = handle_dns_query(&cfg, &tx, guest_mac, guest_ip, src_port, query, raw_query).await {
                                                    warn!(?err, "dns query failed");
                                                }
                                            });
                                        }
                                    }
                                }
                                IP_PROTO_TCP => {
                                    let Some(tcp) = parse_tcp_segment(ip.src, ip.dst, ip.payload) else { continue };
                                    let key = TcpConnKey { guest_port: tcp.src_port, remote_ip: ip.dst, remote_port: tcp.dst_port };
                                    if (tcp.flags & (TCP_FLAG_SYN | TCP_FLAG_ACK)) == TCP_FLAG_SYN {
                                        if tcp_conns.contains_key(&key) {
                                            continue;
                                        }
                                        let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(128);
                                        let stack_isn = rand::thread_rng().gen::<u32>();
                                        let conn = TcpConn {
                                            guest_mac: eth.src,
                                            guest_ip: ip.src,
                                            state: TcpConnState::Connecting { guest_isn: tcp.seq },
                                            guest_next_seq: tcp.seq.wrapping_add(1),
                                            stack_next_seq: stack_isn.wrapping_add(1),
                                            stack_isn,
                                            outbound_tx: Some(outbound_tx),
                                        };
                                        tcp_conns.insert(key, conn);
                                        active_tcp_connections_task.fetch_add(1, Ordering::SeqCst);
                                        tokio::spawn(run_remote_ws(
                                            cfg.proxy_ws_addr,
                                            key,
                                            outbound_rx,
                                            remote_event_tx.clone(),
                                        ));
                                        debug!(?key, "new TCP connection");
                                    } else if let Some(conn) = tcp_conns.get_mut(&key) {
                                        match conn.state {
                                            TcpConnState::SynAckSent { guest_isn } => {
                                                if (tcp.flags & TCP_FLAG_ACK) != 0 && tcp.seq == guest_isn.wrapping_add(1) && tcp.ack == conn.stack_isn.wrapping_add(1) {
                                                    conn.state = TcpConnState::Established;
                                                    debug!(?key, "TCP established");
                                                }
                                            }
                                            TcpConnState::Established | TcpConnState::Closing => {
                                                if !tcp.payload.is_empty() {
                                                    if tcp.seq == conn.guest_next_seq {
                                                        conn.guest_next_seq = conn.guest_next_seq.wrapping_add(tcp.payload.len() as u32);
                                                        if let Some(outbound_tx) = conn.outbound_tx.as_ref() {
                                                            let _ = outbound_tx.send(tcp.payload.to_vec()).await;
                                                        }
                                                        send_tcp_segment_to_guest(
                                                            &cfg,
                                                            &to_guest,
                                                            conn.guest_mac,
                                                            conn.guest_ip,
                                                            key.remote_ip,
                                                            key.remote_port,
                                                            key.guest_port,
                                                            conn.stack_next_seq,
                                                            conn.guest_next_seq,
                                                            TCP_FLAG_ACK,
                                                            &[],
                                                        )
                                                        .await;
                                                    } else {
                                                        // Out-of-order; ignore.
                                                    }
                                                }

                                                if (tcp.flags & TCP_FLAG_FIN) != 0 && tcp.seq == conn.guest_next_seq {
                                                    conn.guest_next_seq = conn.guest_next_seq.wrapping_add(1);
                                                    send_tcp_segment_to_guest(
                                                        &cfg,
                                                        &to_guest,
                                                        conn.guest_mac,
                                                        conn.guest_ip,
                                                        key.remote_ip,
                                                        key.remote_port,
                                                        key.guest_port,
                                                        conn.stack_next_seq,
                                                        conn.guest_next_seq,
                                                        TCP_FLAG_ACK,
                                                        &[],
                                                    )
                                                    .await;

                                                    // Actively close from our side too.
                                                    let seq = conn.stack_next_seq;
                                                    send_tcp_segment_to_guest(
                                                        &cfg,
                                                        &to_guest,
                                                        conn.guest_mac,
                                                        conn.guest_ip,
                                                        key.remote_ip,
                                                        key.remote_port,
                                                        key.guest_port,
                                                        seq,
                                                        conn.guest_next_seq,
                                                        TCP_FLAG_FIN | TCP_FLAG_ACK,
                                                        &[],
                                                    )
                                                    .await;
                                                    conn.stack_next_seq = conn.stack_next_seq.wrapping_add(1);
                                                    conn.state = TcpConnState::Closing;
                                                    conn.outbound_tx = None;
                                                }

                                                if matches!(conn.state, TcpConnState::Closing) && (tcp.flags & TCP_FLAG_ACK) != 0 {
                                                    // Final ACK from guest, tear down.
                                                    if tcp.ack == conn.stack_next_seq {
                                                        tcp_conns.remove(&key);
                                                        active_tcp_connections_task.fetch_sub(1, Ordering::SeqCst);
                                                        debug!(?key, "TCP closed");
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Best-effort cleanup.
        tcp_conns.clear();
        info!("net stack exited");
    });

    StackHandle {
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        active_tcp_connections,
    }
}

async fn handle_arp(
    cfg: &StackConfig,
    to_guest: &mpsc::Sender<Vec<u8>>,
    guest_mac: MacAddr,
    payload: &[u8],
) {
    let Some(arp) = parse_arp_packet(payload) else {
        return;
    };
    if arp.op != ArpOp::Request {
        return;
    }
    if arp.target_ip != cfg.router_ip && arp.target_ip != cfg.dns_ip {
        return;
    }
    let arp_reply = build_arp_packet(
        ArpOp::Reply,
        cfg.router_mac,
        arp.target_ip,
        guest_mac,
        arp.sender_ip,
    );
    let eth = build_ethernet_frame(guest_mac, cfg.router_mac, ETHERTYPE_ARP, &arp_reply);
    let _ = to_guest.send(eth).await;
}

async fn handle_dhcp(
    cfg: &StackConfig,
    to_guest: &mpsc::Sender<Vec<u8>>,
    src_port: u16,
    dst_port: u16,
    parsed: crate::protocol::DhcpParsed,
) {
    let (msg_type, response) = match parsed.message_type {
        DhcpMessageType::Discover => (
            DhcpMessageType::Offer,
            build_dhcp_offer(
                parsed.xid,
                parsed.chaddr,
                cfg.lease_ip,
                cfg.router_ip,
                cfg.netmask,
                cfg.router_ip,
                cfg.dns_ip,
                3600,
            ),
        ),
        DhcpMessageType::Request => {
            // Validate requested IP to keep things simple.
            if parsed.requested_ip != Some(cfg.lease_ip) || parsed.server_id != Some(cfg.router_ip)
            {
                return;
            }
            (
                DhcpMessageType::Ack,
                build_dhcp_ack(
                    parsed.xid,
                    parsed.chaddr,
                    cfg.lease_ip,
                    cfg.router_ip,
                    cfg.netmask,
                    cfg.router_ip,
                    cfg.dns_ip,
                    3600,
                ),
            )
        }
        _ => return,
    };

    debug!(?msg_type, "dhcp response");
    // DHCP replies are broadcast in our harness.
    let udp = build_udp_packet(
        cfg.router_ip,
        Ipv4Addr::BROADCAST,
        dst_port,
        src_port,
        &response,
    );
    let ip = build_ipv4_packet(cfg.router_ip, Ipv4Addr::BROADCAST, IP_PROTO_UDP, &udp);
    let eth = build_ethernet_frame(MacAddr::BROADCAST, cfg.router_mac, ETHERTYPE_IPV4, &ip);
    let _ = to_guest.send(eth).await;
}

async fn handle_dns_query(
    cfg: &StackConfig,
    to_guest: &mpsc::Sender<Vec<u8>>,
    guest_mac: MacAddr,
    guest_ip: Ipv4Addr,
    guest_port: u16,
    query: DnsQuery,
    raw_query: Vec<u8>,
) -> anyhow::Result<()> {
    // Forward via DNS-over-HTTPS over the WS TCP proxy.
    let doh_resp = doh_request_over_proxy(cfg.proxy_ws_addr, cfg.doh_http_addr, &raw_query).await?;
    let resp = parse_dns_query(&doh_resp)
        .map(|_| doh_resp.clone())
        .unwrap_or_else(|| {
            // If DoH server returned something unexpected, respond NXDOMAIN.
            build_dns_response_nxdomain(query.id, &query)
        });

    let udp = build_udp_packet(cfg.dns_ip, guest_ip, 53, guest_port, &resp);
    let ip = build_ipv4_packet(cfg.dns_ip, guest_ip, IP_PROTO_UDP, &udp);
    let eth = build_ethernet_frame(guest_mac, cfg.router_mac, ETHERTYPE_IPV4, &ip);
    let _ = to_guest.send(eth).await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_tcp_segment_to_guest(
    cfg: &StackConfig,
    to_guest: &mpsc::Sender<Vec<u8>>,
    guest_mac: MacAddr,
    guest_ip: Ipv4Addr,
    remote_ip: Ipv4Addr,
    remote_port: u16,
    guest_port: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
) {
    let tcp = build_tcp_segment(
        remote_ip,
        guest_ip,
        remote_port,
        guest_port,
        seq,
        ack,
        flags,
        65535,
        payload,
    );
    let ip = build_ipv4_packet(remote_ip, guest_ip, IP_PROTO_TCP, &tcp);
    let eth = build_ethernet_frame(guest_mac, cfg.router_mac, ETHERTYPE_IPV4, &ip);
    let _ = to_guest.send(eth).await;
}

async fn run_remote_ws(
    proxy_ws_addr: SocketAddr,
    key: TcpConnKey,
    mut outbound_rx: mpsc::Receiver<Vec<u8>>,
    remote_event_tx: mpsc::Sender<RemoteEvent>,
) {
    let url = format!(
        "ws://{}/tcp?v=1&host={}&port={}",
        proxy_ws_addr, key.remote_ip, key.remote_port
    );
    let connect_res = timeout(
        Duration::from_secs(2),
        tokio_tungstenite::connect_async(url),
    )
    .await;
    let ws = match connect_res {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(err)) => {
            let _ = remote_event_tx
                .send(RemoteEvent::Failed {
                    key,
                    error: err.to_string(),
                })
                .await;
            return;
        }
        Err(_) => {
            let _ = remote_event_tx
                .send(RemoteEvent::Failed {
                    key,
                    error: "connect timeout".to_string(),
                })
                .await;
            return;
        }
    };

    let _ = remote_event_tx.send(RemoteEvent::Connected { key }).await;

    let (mut sink, mut stream) = ws.split();
    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let _ = remote_event_tx.send(RemoteEvent::Data { key, data: data.to_vec() }).await;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = remote_event_tx.send(RemoteEvent::Closed { key }).await;
                        break;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = sink.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        let _ = remote_event_tx.send(RemoteEvent::Failed { key, error: err.to_string() }).await;
                        break;
                    }
                }
            }
            data = outbound_rx.recv() => {
                match data {
                    Some(data) => {
                        if sink.send(Message::Binary(data.into())).await.is_err() {
                            let _ = remote_event_tx.send(RemoteEvent::Closed { key }).await;
                            break;
                        }
                    }
                    None => {
                        let _ = sink.send(Message::Close(None)).await;
                        let _ = remote_event_tx.send(RemoteEvent::Closed { key }).await;
                        break;
                    }
                }
            }
        }
    }
}

async fn doh_request_over_proxy(
    proxy_ws_addr: SocketAddr,
    doh_http_addr: SocketAddr,
    dns_query: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let dns_param = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(dns_query);
    let url = format!(
        "ws://{}/tcp?v=1&host={}&port={}",
        proxy_ws_addr,
        doh_http_addr.ip(),
        doh_http_addr.port()
    );
    let (ws, _) = timeout(
        Duration::from_secs(2),
        tokio_tungstenite::connect_async(url),
    )
    .await??;
    let (mut sink, mut stream) = ws.split();

    let req = format!(
        "GET /dns-query?dns={} HTTP/1.1\r\nHost: {}\r\nAccept: application/dns-message\r\nConnection: close\r\n\r\n",
        dns_param,
        doh_http_addr
    );
    sink.send(Message::Binary(req.into_bytes().into())).await?;

    let mut buf = Vec::new();
    while let Some(msg) = stream.next().await {
        match msg? {
            Message::Binary(chunk) => buf.extend_from_slice(&chunk),
            Message::Close(_) => break,
            _ => {}
        }
    }

    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("invalid http response"))?;
    let (headers, body) = buf.split_at(header_end + 4);
    let headers_str = std::str::from_utf8(headers)?;
    if !headers_str.starts_with("HTTP/1.1 200") && !headers_str.starts_with("HTTP/1.0 200") {
        return Err(anyhow::anyhow!("http non-200: {headers_str:?}"));
    }
    Ok(body.to_vec())
}
