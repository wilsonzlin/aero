use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{SystemTime, UNIX_EPOCH},
};

use aero_net_stack::{
    Action, DnsResolved, IpCidr, Millis, NetworkStack, StackConfig, TcpProxyEvent, UdpProxyEvent,
};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};
use tracing::Instrument;

use crate::{overrides::ForwardKey, server::AppState};

// Default-deny private/reserved ranges in the stack itself so we can drop obviously-invalid
// connections early (before creating any tokio socket state).
//
// NOTE: This list intentionally excludes TEST-NET blocks (192.0.2.0/24, 198.51.100.0/24,
// 203.0.113.0/24) so deterministic CI can use those addresses with the test-mode forward maps.
const STACK_DEFAULT_DENY_IPV4: &[IpCidr] = &[
    IpCidr::new(Ipv4Addr::new(0, 0, 0, 0), 8),
    IpCidr::new(Ipv4Addr::new(10, 0, 0, 0), 8),
    IpCidr::new(Ipv4Addr::new(100, 64, 0, 0), 10),
    IpCidr::new(Ipv4Addr::new(127, 0, 0, 0), 8),
    IpCidr::new(Ipv4Addr::new(169, 254, 0, 0), 16),
    IpCidr::new(Ipv4Addr::new(172, 16, 0, 0), 12),
    IpCidr::new(Ipv4Addr::new(192, 168, 0, 0), 16),
    IpCidr::new(Ipv4Addr::new(192, 0, 0, 0), 24),
    IpCidr::new(Ipv4Addr::new(198, 18, 0, 0), 15),
    IpCidr::new(Ipv4Addr::new(224, 0, 0, 0), 4),
    IpCidr::new(Ipv4Addr::new(240, 0, 0, 0), 4),
];

#[derive(Debug)]
enum TcpOutMsg {
    Data(Vec<u8>),
    Close,
}

#[derive(Debug)]
struct TcpConnHandle {
    tx: mpsc::Sender<TcpOutMsg>,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UdpKey {
    guest_port: u16,
    dst_ip: Ipv4Addr,
    dst_port: u16,
}

#[derive(Debug)]
struct UdpFlowHandle {
    socket: std::sync::Arc<UdpSocket>,
    task: JoinHandle<()>,
}

#[derive(Debug)]
enum SessionEvent {
    Tcp(TcpProxyEvent),
    Udp(UdpProxyEvent),
    Dns(DnsResolved),
}

pub(crate) async fn run_session(
    socket: WebSocket,
    state: AppState,
    session_id: u64,
) -> anyhow::Result<()> {
    run_session_inner(socket, state, session_id)
        .instrument(tracing::info_span!("l2_session", session_id))
        .await
}

async fn run_session_inner(
    socket: WebSocket,
    state: AppState,
    session_id: u64,
) -> anyhow::Result<()> {
    state.metrics.session_opened();
    let _session_guard = SessionGuard::new(state.metrics.clone());

    tracing::info!("session opened");

    let mut capture = match state.capture.open_session(session_id).await {
        Ok(capture) => capture,
        Err(err) => {
            tracing::warn!("failed to initialise capture: {err}");
            None
        }
    };

    let (ws_sender, mut ws_receiver) = socket.split();

    let (ws_out_tx, mut ws_out_rx) = mpsc::channel::<Message>(state.cfg.ws_send_buffer);
    let ws_writer = tokio::spawn(async move {
        let mut ws_sender = ws_sender;
        while let Some(msg) = ws_out_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let (event_tx, mut event_rx) = mpsc::channel::<SessionEvent>(128);

    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    if !state.cfg.policy.allow_private_ips() {
        cfg.host_policy
            .deny_ips
            .extend_from_slice(STACK_DEFAULT_DENY_IPV4);
    }
    // This service always fulfills UDP proxy actions using tokio `UdpSocket`s (no WebRTC relay),
    // so ensure the stack labels outbound UDP actions as `UdpTransport::Proxy`.
    cfg.webrtc_udp = false;
    let mut stack = NetworkStack::new(cfg);

    let mut tcp_conns: HashMap<u32, TcpConnHandle> = HashMap::new();
    let mut udp_flows: HashMap<UdpKey, UdpFlowHandle> = HashMap::new();

    let start = tokio::time::Instant::now();
    let mut fatal_err: Option<anyhow::Error> = None;

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                let Some(msg) = msg else {
                    break;
                };
                let Ok(msg) = msg else {
                    break;
                };

                match msg {
                    Message::Binary(data) => {
                        let now_ms = elapsed_ms(start);
                        match aero_l2_protocol::decode_with_limits(&data, &state.l2_limits) {
                            Ok(decoded) => {
                                match decoded.msg_type {
                                    aero_l2_protocol::L2_TUNNEL_TYPE_FRAME => {
                                        let frame = decoded.payload;
                                        state.metrics.frame_rx(frame.len());

                                        let ts_in = now_unix_timestamp_ns();
                                        if let Some(capture) = capture.as_mut() {
                                            let _ = capture.record_guest_to_proxy(ts_in, frame).await;
                                        }

                                        let actions = stack.process_outbound_ethernet(frame, now_ms);
                                        if let Err(err) = process_actions(
                                            &mut stack,
                                            actions,
                                            now_ms,
                                            &ws_out_tx,
                                            &event_tx,
                                            &mut tcp_conns,
                                            &mut udp_flows,
                                            &mut capture,
                                            &state,
                                        )
                                        .await
                                        {
                                            fatal_err = Some(err);
                                            break;
                                        }
                                    }
                                    aero_l2_protocol::L2_TUNNEL_TYPE_PING => {
                                        if let Ok(pong) = aero_l2_protocol::encode_with_limits(
                                            aero_l2_protocol::L2_TUNNEL_TYPE_PONG,
                                            0,
                                            decoded.payload,
                                            &state.l2_limits,
                                        ) {
                                            let _ = ws_out_tx.send(Message::Binary(pong)).await;
                                        }
                                    }
                                    aero_l2_protocol::L2_TUNNEL_TYPE_PONG => {}
                                    _ => {}
                                }
                            }
                            Err(err) => {
                                state.metrics.frame_dropped();
                                tracing::debug!("dropping invalid l2 message: {err}");
                                let msg = err.to_string();
                                let payload = msg.as_bytes();
                                let payload = if payload.len() > state.l2_limits.max_control_payload {
                                    &payload[..state.l2_limits.max_control_payload]
                                } else {
                                    payload
                                };

                                if let Ok(wire) = aero_l2_protocol::encode_with_limits(
                                    aero_l2_protocol::L2_TUNNEL_TYPE_ERROR,
                                    0,
                                    payload,
                                    &state.l2_limits,
                                ) {
                                    let _ = ws_out_tx.send(Message::Binary(wire)).await;
                                }
                                break;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        let _ = ws_out_tx.send(Message::Pong(payload)).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            event = event_rx.recv() => {
                let Some(event) = event else {
                    break;
                };
                let now_ms = elapsed_ms(start);

                match &event {
                    SessionEvent::Tcp(TcpProxyEvent::Closed { connection_id } | TcpProxyEvent::Error { connection_id }) => {
                        if let Some(handle) = tcp_conns.remove(connection_id) {
                            handle.task.abort();
                            state.metrics.tcp_conn_closed();
                        }
                    }
                    _ => {}
                }

                let actions = match event {
                    SessionEvent::Tcp(ev) => stack.handle_tcp_proxy_event(ev, now_ms),
                    SessionEvent::Udp(ev) => stack.handle_udp_proxy_event(ev, now_ms),
                    SessionEvent::Dns(ev) => stack.handle_dns_resolved(ev, now_ms),
                };

                if let Err(err) = process_actions(
                    &mut stack,
                    actions,
                    now_ms,
                    &ws_out_tx,
                    &event_tx,
                    &mut tcp_conns,
                    &mut udp_flows,
                    &mut capture,
                    &state,
                )
                .await
                {
                    fatal_err = Some(err);
                    break;
                }
            }
        }
    }

    for (_, conn) in tcp_conns {
        conn.task.abort();
        state.metrics.tcp_conn_closed();
    }
    for (_, flow) in udp_flows {
        flow.task.abort();
        state.metrics.udp_flow_closed();
    }

    drop(ws_out_tx);
    ws_writer.abort();

    if let Some(capture) = capture {
        let path = capture.path().to_path_buf();
        if let Err(err) = capture.close().await {
            tracing::warn!("failed to flush capture file: {err}");
        } else {
            tracing::info!(path = ?path, "wrote capture file");
        }
    }

    tracing::info!("session closed");

    match fatal_err {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

fn elapsed_ms(start: tokio::time::Instant) -> Millis {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

struct SessionGuard {
    metrics: crate::metrics::Metrics,
}

impl SessionGuard {
    fn new(metrics: crate::metrics::Metrics) -> Self {
        Self { metrics }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.metrics.session_closed();
    }
}

fn now_unix_timestamp_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(dur) => duration_to_ns(dur),
        Err(err) => duration_to_ns(err.duration()),
    }
}

fn duration_to_ns(dur: std::time::Duration) -> u64 {
    dur.as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(dur.subsec_nanos()))
}

async fn process_actions(
    stack: &mut NetworkStack,
    actions: Vec<Action>,
    now_ms: Millis,
    ws_out_tx: &mpsc::Sender<Message>,
    event_tx: &mpsc::Sender<SessionEvent>,
    tcp_conns: &mut HashMap<u32, TcpConnHandle>,
    udp_flows: &mut HashMap<UdpKey, UdpFlowHandle>,
    capture: &mut Option<crate::capture::SessionCapture>,
    state: &AppState,
) -> anyhow::Result<()> {
    let mut queue: VecDeque<Action> = actions.into();
    while let Some(action) = queue.pop_front() {
        match action {
            Action::EmitFrame(frame) => {
                let Ok(wire) = aero_l2_protocol::encode_with_limits(
                    aero_l2_protocol::L2_TUNNEL_TYPE_FRAME,
                    0,
                    &frame,
                    &state.l2_limits,
                ) else {
                    state.metrics.frame_dropped();
                    continue;
                };

                let ts_out = now_unix_timestamp_ns();
                if let Some(capture) = capture.as_mut() {
                    let _ = capture.record_proxy_to_guest(ts_out, &frame).await;
                }
                if ws_out_tx.send(Message::Binary(wire)).await.is_err() {
                    return Ok(());
                }
                state.metrics.frame_tx(frame.len());
            }
            Action::TcpProxyConnect {
                connection_id,
                remote_ip,
                remote_port,
            } => {
                if tcp_conns.contains_key(&connection_id) {
                    continue;
                }

                let forward_key = ForwardKey {
                    ip: remote_ip,
                    port: remote_port,
                };
                let forward = state
                    .cfg
                    .test_overrides
                    .tcp_forward
                    .get(&forward_key)
                    .cloned();
                if forward.is_none()
                    && (!state.cfg.policy.allows_ip(remote_ip)
                        || !state.cfg.policy.allows_tcp_port(remote_port))
                {
                    state.metrics.policy_denied();
                    queue.extend(
                        stack
                            .handle_tcp_proxy_event(TcpProxyEvent::Error { connection_id }, now_ms),
                    );
                    continue;
                }

                let (tx, rx) = mpsc::channel::<TcpOutMsg>(state.cfg.tcp_send_buffer);
                let event_tx = event_tx.clone();
                let metrics = state.metrics.clone();
                let timeout_dur = state.cfg.tcp_connect_timeout;

                let target = forward
                    .map(|f| (f.host, f.port))
                    .unwrap_or_else(|| (remote_ip.to_string(), remote_port));

                let task = tokio::spawn(async move {
                    tcp_task(connection_id, target, rx, event_tx, timeout_dur, metrics).await;
                });

                tcp_conns.insert(connection_id, TcpConnHandle { tx, task });
                state.metrics.tcp_conn_opened();
            }
            Action::TcpProxySend {
                connection_id,
                data,
            } => {
                let Some(handle) = tcp_conns.get(&connection_id) else {
                    continue;
                };

                if handle.tx.try_send(TcpOutMsg::Data(data)).is_err() {
                    if let Some(handle) = tcp_conns.remove(&connection_id) {
                        handle.task.abort();
                    }
                    queue.extend(
                        stack
                            .handle_tcp_proxy_event(TcpProxyEvent::Error { connection_id }, now_ms),
                    );
                }
            }
            Action::TcpProxyClose { connection_id } => {
                let Some(handle) = tcp_conns.get(&connection_id) else {
                    continue;
                };
                let _ = handle.tx.try_send(TcpOutMsg::Close);
            }
            Action::UdpProxySend {
                transport: _,
                src_port,
                dst_ip,
                dst_port,
                data,
            } => {
                let forward_key = ForwardKey {
                    ip: dst_ip,
                    port: dst_port,
                };
                let forward = state
                    .cfg
                    .test_overrides
                    .udp_forward
                    .get(&forward_key)
                    .cloned();
                if forward.is_none()
                    && (!state.cfg.policy.allows_ip(dst_ip)
                        || !state.cfg.policy.allows_udp_port(dst_port))
                {
                    state.metrics.policy_denied();
                    continue;
                }

                let key = UdpKey {
                    guest_port: src_port,
                    dst_ip,
                    dst_port,
                };

                if !udp_flows.contains_key(&key) {
                    let remote = forward
                        .map(|f| (f.host, f.port))
                        .unwrap_or_else(|| (dst_ip.to_string(), dst_port));
                    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
                    let remote_addr = resolve_host_port(&remote.0, remote.1).await?;
                    socket.connect(remote_addr).await?;
                    let socket = std::sync::Arc::new(socket);
                    let socket_task = socket.clone();
                    let event_tx = event_tx.clone();
                    let task = tokio::spawn(async move {
                        udp_task(key, socket_task, event_tx).await;
                    });
                    udp_flows.insert(key, UdpFlowHandle { socket, task });
                    state.metrics.udp_flow_opened();
                }

                if let Some(flow) = udp_flows.get(&key) {
                    if flow.socket.send(&data).await.is_err() {
                        state.metrics.udp_send_failed();
                    }
                }
            }
            Action::DnsResolve { request_id, name } => {
                state.metrics.dns_query();
                if !state.cfg.policy.allows_domain(&name) {
                    state.metrics.policy_denied();
                    queue.extend(stack.handle_dns_resolved(
                        DnsResolved {
                            request_id,
                            name,
                            addr: None,
                            ttl_secs: 0,
                        },
                        now_ms,
                    ));
                    continue;
                }

                let dns = state.dns.clone();
                let event_tx = event_tx.clone();
                let policy = state.cfg.policy.clone();
                let name_task = name.clone();
                let metrics = state.metrics.clone();

                tokio::spawn(async move {
                    let resolved = match dns.resolve_ipv4(&name_task).await {
                        Ok((addr, ttl, is_override)) => {
                            if addr.is_none() {
                                metrics.dns_fail();
                            }

                            let filtered = addr.filter(|ip| is_override || policy.allows_ip(*ip));
                            if addr.is_some() && filtered.is_none() && !is_override {
                                metrics.policy_denied();
                            }
                            DnsResolved {
                                request_id,
                                name: name_task,
                                addr: filtered,
                                ttl_secs: ttl,
                            }
                        }
                        Err(_) => {
                            metrics.dns_fail();
                            DnsResolved {
                                request_id,
                                name: name_task,
                                addr: None,
                                ttl_secs: 0,
                            }
                        }
                    };

                    let _ = event_tx.send(SessionEvent::Dns(resolved)).await;
                });
            }
        }
    }

    Ok(())
}

async fn tcp_task(
    connection_id: u32,
    target: (String, u16),
    mut rx: mpsc::Receiver<TcpOutMsg>,
    event_tx: mpsc::Sender<SessionEvent>,
    connect_timeout: std::time::Duration,
    metrics: crate::metrics::Metrics,
) {
    let addr = match resolve_host_port(&target.0, target.1).await {
        Ok(addr) => addr,
        Err(_) => {
            metrics.tcp_connect_failed();
            let _ = event_tx
                .send(SessionEvent::Tcp(TcpProxyEvent::Error { connection_id }))
                .await;
            return;
        }
    };

    let stream = match timeout(connect_timeout, TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => stream,
        _ => {
            metrics.tcp_connect_failed();
            let _ = event_tx
                .send(SessionEvent::Tcp(TcpProxyEvent::Error { connection_id }))
                .await;
            return;
        }
    };

    let _ = stream.set_nodelay(true);
    let (mut reader, mut writer) = stream.into_split();

    let _ = event_tx
        .send(SessionEvent::Tcp(TcpProxyEvent::Connected {
            connection_id,
        }))
        .await;

    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            read_res = reader.read(&mut buf) => {
                match read_res {
                    Ok(0) => {
                        let _ = event_tx
                            .send(SessionEvent::Tcp(TcpProxyEvent::Closed { connection_id }))
                            .await;
                        break;
                    }
                    Ok(n) => {
                        if event_tx
                            .send(SessionEvent::Tcp(TcpProxyEvent::Data { connection_id, data: buf[..n].to_vec() }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = event_tx
                            .send(SessionEvent::Tcp(TcpProxyEvent::Error { connection_id }))
                            .await;
                        break;
                    }
                }
            }
            msg = rx.recv() => {
                match msg {
                    Some(TcpOutMsg::Data(data)) => {
                        if writer.write_all(&data).await.is_err() {
                            let _ = event_tx
                                .send(SessionEvent::Tcp(TcpProxyEvent::Error { connection_id }))
                                .await;
                            break;
                        }
                    }
                    Some(TcpOutMsg::Close) | None => {
                        let _ = writer.shutdown().await;
                        let _ = event_tx
                            .send(SessionEvent::Tcp(TcpProxyEvent::Closed { connection_id }))
                            .await;
                        break;
                    }
                }
            }
        }
    }
}

async fn udp_task(
    key: UdpKey,
    socket: std::sync::Arc<UdpSocket>,
    event_tx: mpsc::Sender<SessionEvent>,
) {
    let mut buf = vec![0u8; 2048];
    loop {
        let n = match socket.recv(&mut buf).await {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            continue;
        }
        let event = UdpProxyEvent {
            src_ip: key.dst_ip,
            src_port: key.dst_port,
            dst_port: key.guest_port,
            data: buf[..n].to_vec(),
        };
        if event_tx.send(SessionEvent::Udp(event)).await.is_err() {
            break;
        }
    }
}

async fn resolve_host_port(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    // Allow direct numeric IPs without DNS lookups.
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }

    let mut addrs = tokio::net::lookup_host((host, port)).await?;
    addrs
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses found"))
}
