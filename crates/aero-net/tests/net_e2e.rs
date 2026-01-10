use std::{
    collections::HashMap,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use aero_net::{
    protocol::{
        build_arp_packet, build_dhcp_discover, build_dhcp_request, build_dns_response_a,
        build_dns_response_nxdomain, build_ethernet_frame, build_ipv4_packet, build_tcp_segment,
        build_udp_packet, parse_arp_packet, parse_dhcp, parse_ethernet_frame, parse_ipv4_packet,
        parse_tcp_segment, parse_udp_packet, ArpOp, DhcpMessageType, MacAddr, ETHERTYPE_ARP,
        ETHERTYPE_IPV4, IP_PROTO_TCP, IP_PROTO_UDP, TCP_FLAG_ACK, TCP_FLAG_FIN, TCP_FLAG_PSH,
        TCP_FLAG_SYN,
    },
    stack::{spawn_stack, StackConfig},
};
use aero_net_proxy_server::{start_proxy_server, ProxyServerOptions};
use base64::Engine;
use futures_util::FutureExt;
use emulator::io::net::trace::{CaptureArtifactOnPanic, FrameDirection, NetTraceConfig, NetTracer};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn net_e2e() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    let trace = Trace::default();
    let tracer = Arc::new(NetTracer::new(NetTraceConfig::default()));
    tracer.enable();

    let res = std::panic::AssertUnwindSafe(run_net_e2e(trace.clone(), tracer.clone()))
        .catch_unwind()
        .await;
    if let Err(panic) = res {
        trace.dump("net_e2e");
        // If the panic occurred before `CaptureArtifactOnPanic` was constructed, still emit an
        // artifact from the frames that were recorded.
        let _ = std::fs::create_dir_all("target/nt-test-artifacts");
        let _ = std::fs::write("target/nt-test-artifacts/net_e2e.pcapng", tracer.export_pcapng());
        std::panic::resume_unwind(panic);
    }
}

async fn run_net_e2e(trace: Trace, tracer: Arc<NetTracer>) {
    let _capture_guard = CaptureArtifactOnPanic::for_test(tracer.as_ref(), "net_e2e");

    let echo = TcpEchoServer::spawn().await;
    let doh = DohHttpServer::spawn(HashMap::from([(
        "echo.local".to_string(),
        Ipv4Addr::new(127, 0, 0, 1),
    )]))
    .await;

    let proxy = start_proxy_server(ProxyServerOptions::default())
        .await
        .expect("proxy server start");

    let cfg = StackConfig {
        router_mac: MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
        router_ip: Ipv4Addr::new(10, 0, 2, 2),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        lease_ip: Ipv4Addr::new(10, 0, 2, 15),
        dns_ip: Ipv4Addr::new(10, 0, 2, 2),
        proxy_ws_addr: proxy.local_addr(),
        doh_http_addr: doh.addr,
    };

    let (guest_to_stack_tx, guest_to_stack_rx) = mpsc::channel::<Vec<u8>>(2048);
    let (stack_to_guest_tx, mut stack_to_guest_rx) = mpsc::channel::<Vec<u8>>(2048);
    let stack = spawn_stack(cfg.clone(), guest_to_stack_rx, stack_to_guest_tx);

    let guest_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);

    // DHCP handshake.
    let xid = 0x1234_5678;
    trace.log(format!("dhcp: xid={xid:#x} sending DISCOVER"));
    send_dhcp_discover(tracer.as_ref(), &trace, &guest_to_stack_tx, guest_mac, xid).await;

    let offer =
        recv_dhcp(tracer.as_ref(), &trace, &mut stack_to_guest_rx, DhcpMessageType::Offer).await;
    assert_eq!(offer.yiaddr, cfg.lease_ip);
    trace.log(format!("dhcp: got OFFER yiaddr={}", offer.yiaddr));

    trace.log("dhcp: sending REQUEST".to_string());
    send_dhcp_request(
        tracer.as_ref(),
        &trace,
        &guest_to_stack_tx,
        guest_mac,
        xid,
        cfg.lease_ip,
        cfg.router_ip,
    )
    .await;
    let ack =
        recv_dhcp(tracer.as_ref(), &trace, &mut stack_to_guest_rx, DhcpMessageType::Ack).await;
    assert_eq!(ack.yiaddr, cfg.lease_ip);
    trace.log(format!("dhcp: got ACK yiaddr={}", ack.yiaddr));

    // ARP for router.
    trace.log("arp: requesting router mac".to_string());
    send_arp_request(
        tracer.as_ref(),
        &trace,
        &guest_to_stack_tx,
        guest_mac,
        cfg.lease_ip,
        cfg.router_ip,
    )
    .await;
    let router_mac =
        recv_arp_reply(tracer.as_ref(), &trace, &mut stack_to_guest_rx, cfg.router_ip).await;
    assert_eq!(router_mac, cfg.router_mac);

    // DNS query (guest -> stack -> DoH over proxy -> stack -> guest).
    let dns_id = 0xBEEF;
    trace.log("dns: querying echo.local".to_string());
    let dns_query = build_dns_query(dns_id, "echo.local");
    send_udp(
        tracer.as_ref(),
        &trace,
        &guest_to_stack_tx,
        guest_mac,
        router_mac,
        cfg.lease_ip,
        cfg.dns_ip,
        12000,
        53,
        &dns_query,
    )
    .await;
    let dns_resp = recv_udp_payload(
        tracer.as_ref(),
        &trace,
        &mut stack_to_guest_rx,
        cfg.dns_ip,
        cfg.lease_ip,
        53,
        12000,
    )
    .await;
    let dns_answer = parse_dns_response_a(&dns_resp).expect("dns A answer");
    assert_eq!(dns_answer, Ipv4Addr::new(127, 0, 0, 1));
    trace.log(format!("dns: answer = {dns_answer}"));

    // Two simultaneous TCP connections to the echo server.
    let echo_ip = dns_answer;
    let port_a = 40000;
    let port_b = 40001;
    let (conn_a_tx, conn_a_rx) = mpsc::channel::<Vec<u8>>(256);
    let (conn_b_tx, conn_b_rx) = mpsc::channel::<Vec<u8>>(256);

    let (dispatch_shutdown_tx, dispatch_shutdown_rx) = oneshot::channel::<()>();
    let dispatcher = tokio::spawn(dispatch_frames(
        tracer.clone(),
        stack_to_guest_rx,
        cfg.lease_ip,
        port_a,
        conn_a_tx,
        port_b,
        conn_b_tx,
        dispatch_shutdown_rx,
    ));

    let a = tokio::spawn(tcp_echo_roundtrip(
        trace.clone(),
        tracer.clone(),
        guest_to_stack_tx.clone(),
        guest_mac,
        router_mac,
        cfg.lease_ip,
        echo_ip,
        echo.addr.port(),
        port_a,
        b"conn-a:hello".to_vec(),
        conn_a_rx,
    ));
    let b = tokio::spawn(tcp_echo_roundtrip(
        trace.clone(),
        tracer.clone(),
        guest_to_stack_tx.clone(),
        guest_mac,
        router_mac,
        cfg.lease_ip,
        echo_ip,
        echo.addr.port(),
        port_b,
        b"conn-b:world".to_vec(),
        conn_b_rx,
    ));

    let (a_res, b_res) = timeout(Duration::from_secs(10), async { tokio::join!(a, b) })
        .await
        .expect("tcp tasks timeout");
    a_res.expect("conn a task panicked");
    b_res.expect("conn b task panicked");

    wait_for_zero("stack tcp", || stack.active_tcp_connections()).await;
    wait_for_zero("proxy connections", || proxy.active_connections()).await;
    wait_for_zero("echo connections", || echo.active_connections()).await;
    wait_for_zero("doh connections", || doh.active_connections()).await;

    // Teardown.
    let _ = dispatch_shutdown_tx.send(());
    let _ = dispatcher.await;
    stack.shutdown().await;
    proxy.shutdown().await;
    echo.shutdown().await;
    doh.shutdown().await;
}

#[derive(Clone, Default)]
struct Trace {
    entries: Arc<Mutex<Vec<String>>>,
}

impl Trace {
    fn log(&self, msg: String) {
        self.entries.lock().expect("lock").push(msg);
    }

    fn dump(&self, name: &str) {
        let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .to_path_buf();
        let dir = workspace_root.join("target").join("net-traces");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.log"));
        if let Ok(mut f) = std::fs::File::create(&path) {
            let entries = self.entries.lock().expect("lock");
            for line in entries.iter() {
                let _ = writeln!(f, "{line}");
            }
        }
        eprintln!("wrote trace to {path:?}");
    }
}

async fn wait_for_zero(name: &str, f: impl Fn() -> usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = f();
        if v == 0 {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("{name} did not drain (still {v})");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn dispatch_frames(
    tracer: Arc<NetTracer>,
    mut stack_to_guest_rx: mpsc::Receiver<Vec<u8>>,
    guest_ip: Ipv4Addr,
    port_a: u16,
    conn_a_tx: mpsc::Sender<Vec<u8>>,
    port_b: u16,
    conn_b_tx: mpsc::Sender<Vec<u8>>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            frame = stack_to_guest_rx.recv() => {
                let Some(frame) = frame else { break };
                tracer.record_ethernet(FrameDirection::GuestRx, &frame);
                let Some(eth) = parse_ethernet_frame(&frame) else { continue };
                if eth.ethertype != ETHERTYPE_IPV4 { continue; }
                let Some(ip) = parse_ipv4_packet(eth.payload) else { continue };
                if ip.protocol != IP_PROTO_TCP { continue; }
                if ip.dst != guest_ip { continue; }
                let Some(tcp) = parse_tcp_segment(ip.src, ip.dst, ip.payload) else { continue };
                let _ = match tcp.dst_port {
                    p if p == port_a => conn_a_tx.send(frame).await,
                    p if p == port_b => conn_b_tx.send(frame).await,
                    _ => Ok(()),
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn tcp_echo_roundtrip(
    trace: Trace,
    tracer: Arc<NetTracer>,
    guest_to_stack_tx: mpsc::Sender<Vec<u8>>,
    guest_mac: MacAddr,
    router_mac: MacAddr,
    guest_ip: Ipv4Addr,
    remote_ip: Ipv4Addr,
    remote_port: u16,
    guest_port: u16,
    payload: Vec<u8>,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    let guest_isn = 1_000_000 + guest_port as u32;
    trace.log(format!(
        "tcp: connect {}:{} from {} (port {})",
        remote_ip, remote_port, guest_ip, guest_port
    ));

    send_tcp(
        tracer.as_ref(),
        &trace,
        &guest_to_stack_tx,
        guest_mac,
        router_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_isn,
        0,
        TCP_FLAG_SYN,
        &[],
    )
    .await;

    let syn_ack = recv_tcp(
        &trace,
        &mut rx,
        remote_ip,
        guest_ip,
        remote_port,
        guest_port,
        TCP_FLAG_SYN | TCP_FLAG_ACK,
    )
    .await;
    assert_eq!(syn_ack.ack, guest_isn.wrapping_add(1));
    let mut remote_next_seq = syn_ack.seq.wrapping_add(1);
    let mut guest_next_seq = guest_isn.wrapping_add(1);

    send_tcp(
        tracer.as_ref(),
        &trace,
        &guest_to_stack_tx,
        guest_mac,
        router_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_next_seq,
        remote_next_seq,
        TCP_FLAG_ACK,
        &[],
    )
    .await;

    // Send payload.
    send_tcp(
        tracer.as_ref(),
        &trace,
        &guest_to_stack_tx,
        guest_mac,
        router_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_next_seq,
        remote_next_seq,
        TCP_FLAG_ACK | TCP_FLAG_PSH,
        &payload,
    )
    .await;
    guest_next_seq = guest_next_seq.wrapping_add(payload.len() as u32);

    // Read echoed payload (might be split).
    let mut got = Vec::new();
    while got.len() < payload.len() {
        let seg = recv_any_tcp(
            &trace,
            &mut rx,
            remote_ip,
            guest_ip,
            remote_port,
            guest_port,
        )
        .await;
        if !seg.payload.is_empty() {
            assert_eq!(seg.seq, remote_next_seq);
            got.extend_from_slice(&seg.payload);
            remote_next_seq = remote_next_seq.wrapping_add(seg.payload.len() as u32);
            send_tcp(
                tracer.as_ref(),
                &trace,
                &guest_to_stack_tx,
                guest_mac,
                router_mac,
                guest_ip,
                remote_ip,
                guest_port,
                remote_port,
                guest_next_seq,
                remote_next_seq,
                TCP_FLAG_ACK,
                &[],
            )
            .await;
        }
    }
    assert_eq!(got, payload);

    // Close (FIN handshake).
    send_tcp(
        tracer.as_ref(),
        &trace,
        &guest_to_stack_tx,
        guest_mac,
        router_mac,
        guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        guest_next_seq,
        remote_next_seq,
        TCP_FLAG_FIN | TCP_FLAG_ACK,
        &[],
    )
    .await;
    guest_next_seq = guest_next_seq.wrapping_add(1);

    // We expect ACK then FIN|ACK (possibly in either order).
    let mut got_fin = false;
    while !got_fin {
        let seg = recv_any_tcp(
            &trace,
            &mut rx,
            remote_ip,
            guest_ip,
            remote_port,
            guest_port,
        )
        .await;
        if (seg.flags & TCP_FLAG_FIN) != 0 {
            assert_eq!(seg.seq, remote_next_seq);
            remote_next_seq = remote_next_seq.wrapping_add(1);
            send_tcp(
                tracer.as_ref(),
                &trace,
                &guest_to_stack_tx,
                guest_mac,
                router_mac,
                guest_ip,
                remote_ip,
                guest_port,
                remote_port,
                guest_next_seq,
                remote_next_seq,
                TCP_FLAG_ACK,
                &[],
            )
            .await;
            got_fin = true;
        }
    }
    trace.log(format!("tcp: closed port {guest_port}"));
}

async fn recv_dhcp(
    tracer: &NetTracer,
    trace: &Trace,
    rx: &mut mpsc::Receiver<Vec<u8>>,
    expected: DhcpMessageType,
) -> aero_net::protocol::DhcpParsed {
    let frame = recv_frame_with_timeout(rx).await;
    tracer.record_ethernet(FrameDirection::GuestRx, &frame);
    trace.log(format!("rx: dhcp frame {} bytes", frame.len()));
    let eth = parse_ethernet_frame(&frame).expect("ethernet");
    assert_eq!(eth.ethertype, ETHERTYPE_IPV4);
    let ip = parse_ipv4_packet(eth.payload).expect("ipv4");
    assert_eq!(ip.protocol, IP_PROTO_UDP);
    let udp = parse_udp_packet(ip.src, ip.dst, ip.payload).expect("udp");
    assert_eq!(udp.src_port, 67);
    assert_eq!(udp.dst_port, 68);
    let dhcp = parse_dhcp(udp.payload).expect("dhcp");
    assert_eq!(dhcp.message_type, expected);
    dhcp
}

async fn recv_arp_reply(
    tracer: &NetTracer,
    trace: &Trace,
    rx: &mut mpsc::Receiver<Vec<u8>>,
    expected_sender_ip: Ipv4Addr,
) -> MacAddr {
    let frame = recv_frame_with_timeout(rx).await;
    tracer.record_ethernet(FrameDirection::GuestRx, &frame);
    trace.log(format!("rx: arp frame {} bytes", frame.len()));
    let eth = parse_ethernet_frame(&frame).expect("ethernet");
    assert_eq!(eth.ethertype, ETHERTYPE_ARP);
    let arp = parse_arp_packet(eth.payload).expect("arp");
    assert_eq!(arp.op, ArpOp::Reply);
    assert_eq!(arp.sender_ip, expected_sender_ip);
    arp.sender_mac
}

async fn recv_udp_payload(
    tracer: &NetTracer,
    trace: &Trace,
    rx: &mut mpsc::Receiver<Vec<u8>>,
    expected_src_ip: Ipv4Addr,
    expected_dst_ip: Ipv4Addr,
    expected_src_port: u16,
    expected_dst_port: u16,
) -> Vec<u8> {
    let frame = recv_frame_with_timeout(rx).await;
    tracer.record_ethernet(FrameDirection::GuestRx, &frame);
    trace.log(format!("rx: udp frame {} bytes", frame.len()));
    let eth = parse_ethernet_frame(&frame).expect("ethernet");
    assert_eq!(eth.ethertype, ETHERTYPE_IPV4);
    let ip = parse_ipv4_packet(eth.payload).expect("ipv4");
    assert_eq!(ip.protocol, IP_PROTO_UDP);
    assert_eq!(ip.src, expected_src_ip);
    assert_eq!(ip.dst, expected_dst_ip);
    let udp = parse_udp_packet(ip.src, ip.dst, ip.payload).expect("udp");
    assert_eq!(udp.src_port, expected_src_port);
    assert_eq!(udp.dst_port, expected_dst_port);
    udp.payload.to_vec()
}

async fn recv_tcp(
    trace: &Trace,
    rx: &mut mpsc::Receiver<Vec<u8>>,
    expected_src_ip: Ipv4Addr,
    expected_dst_ip: Ipv4Addr,
    expected_src_port: u16,
    expected_dst_port: u16,
    expected_flags: u16,
) -> OwnedTcpSegment {
    let seg = recv_any_tcp(
        trace,
        rx,
        expected_src_ip,
        expected_dst_ip,
        expected_src_port,
        expected_dst_port,
    )
    .await;
    assert_eq!(seg.flags & expected_flags, expected_flags);
    seg
}

async fn recv_any_tcp(
    trace: &Trace,
    rx: &mut mpsc::Receiver<Vec<u8>>,
    expected_src_ip: Ipv4Addr,
    expected_dst_ip: Ipv4Addr,
    expected_src_port: u16,
    expected_dst_port: u16,
) -> OwnedTcpSegment {
    loop {
        let frame = recv_frame_with_timeout(rx).await;
        trace.log(format!("rx: tcp frame {} bytes", frame.len()));
        let eth = parse_ethernet_frame(&frame).expect("ethernet");
        if eth.ethertype != ETHERTYPE_IPV4 {
            continue;
        }
        let ip = parse_ipv4_packet(eth.payload).expect("ipv4");
        if ip.protocol != IP_PROTO_TCP {
            continue;
        }
        if ip.src != expected_src_ip || ip.dst != expected_dst_ip {
            continue;
        }
        let tcp = parse_tcp_segment(ip.src, ip.dst, ip.payload).expect("tcp");
        if tcp.src_port != expected_src_port || tcp.dst_port != expected_dst_port {
            continue;
        }
        return OwnedTcpSegment {
            seq: tcp.seq,
            ack: tcp.ack,
            flags: tcp.flags,
            payload: tcp.payload.to_vec(),
        };
    }
}

#[derive(Debug)]
struct OwnedTcpSegment {
    seq: u32,
    ack: u32,
    flags: u16,
    payload: Vec<u8>,
}

async fn recv_frame_with_timeout(rx: &mut mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("rx timeout")
        .expect("rx closed")
}

async fn send_dhcp_discover(
    tracer: &NetTracer,
    trace: &Trace,
    tx: &mpsc::Sender<Vec<u8>>,
    mac: MacAddr,
    xid: u32,
) {
    let dhcp = build_dhcp_discover(xid, mac);
    let udp = build_udp_packet(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, 68, 67, &dhcp);
    let ip = build_ipv4_packet(
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        IP_PROTO_UDP,
        &udp,
    );
    let eth = build_ethernet_frame(MacAddr::BROADCAST, mac, ETHERTYPE_IPV4, &ip);
    tracer.record_ethernet(FrameDirection::GuestTx, &eth);
    trace.log(format!("tx: dhcp discover {} bytes", eth.len()));
    tx.send(eth).await.expect("send");
}

async fn send_dhcp_request(
    tracer: &NetTracer,
    trace: &Trace,
    tx: &mpsc::Sender<Vec<u8>>,
    mac: MacAddr,
    xid: u32,
    requested_ip: Ipv4Addr,
    server_ip: Ipv4Addr,
) {
    let dhcp = build_dhcp_request(xid, mac, requested_ip, server_ip);
    let udp = build_udp_packet(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, 68, 67, &dhcp);
    let ip = build_ipv4_packet(
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        IP_PROTO_UDP,
        &udp,
    );
    let eth = build_ethernet_frame(MacAddr::BROADCAST, mac, ETHERTYPE_IPV4, &ip);
    tracer.record_ethernet(FrameDirection::GuestTx, &eth);
    trace.log(format!("tx: dhcp request {} bytes", eth.len()));
    tx.send(eth).await.expect("send");
}

async fn send_arp_request(
    tracer: &NetTracer,
    trace: &Trace,
    tx: &mpsc::Sender<Vec<u8>>,
    mac: MacAddr,
    src_ip: Ipv4Addr,
    target_ip: Ipv4Addr,
) {
    let arp = build_arp_packet(
        ArpOp::Request,
        mac,
        src_ip,
        MacAddr([0, 0, 0, 0, 0, 0]),
        target_ip,
    );
    let eth = build_ethernet_frame(MacAddr::BROADCAST, mac, ETHERTYPE_ARP, &arp);
    tracer.record_ethernet(FrameDirection::GuestTx, &eth);
    trace.log(format!("tx: arp request {} bytes", eth.len()));
    tx.send(eth).await.expect("send");
}

#[allow(clippy::too_many_arguments)]
async fn send_udp(
    tracer: &NetTracer,
    trace: &Trace,
    tx: &mpsc::Sender<Vec<u8>>,
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) {
    let udp = build_udp_packet(src_ip, dst_ip, src_port, dst_port, payload);
    let ip = build_ipv4_packet(src_ip, dst_ip, IP_PROTO_UDP, &udp);
    let eth = build_ethernet_frame(dst_mac, src_mac, ETHERTYPE_IPV4, &ip);
    tracer.record_ethernet(FrameDirection::GuestTx, &eth);
    trace.log(format!(
        "tx: udp {}:{} -> {}:{} ({} bytes)",
        src_ip,
        src_port,
        dst_ip,
        dst_port,
        payload.len()
    ));
    tx.send(eth).await.expect("send");
}

#[allow(clippy::too_many_arguments)]
async fn send_tcp(
    tracer: &NetTracer,
    trace: &Trace,
    tx: &mpsc::Sender<Vec<u8>>,
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
) {
    let tcp = build_tcp_segment(
        src_ip, dst_ip, src_port, dst_port, seq, ack, flags, 65535, payload,
    );
    let ip = build_ipv4_packet(src_ip, dst_ip, IP_PROTO_TCP, &tcp);
    let eth = build_ethernet_frame(dst_mac, src_mac, ETHERTYPE_IPV4, &ip);
    tracer.record_ethernet(FrameDirection::GuestTx, &eth);
    trace.log(format!(
        "tx: tcp {}:{} -> {}:{} flags={flags:#x} seq={seq} ack={ack} len={}",
        src_ip,
        src_port,
        dst_ip,
        dst_port,
        payload.len()
    ));
    tx.send(eth).await.expect("send");
}

fn build_dns_query(id: u16, name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&0u16.to_be_bytes()); // ancount
    out.extend_from_slice(&0u16.to_be_bytes()); // nscount
    out.extend_from_slice(&0u16.to_be_bytes()); // arcount
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&1u16.to_be_bytes()); // type A
    out.extend_from_slice(&1u16.to_be_bytes()); // class IN
    out
}

fn parse_dns_response_a(resp: &[u8]) -> Option<Ipv4Addr> {
    if resp.len() < 12 {
        return None;
    }
    let qdcount = u16::from_be_bytes([resp[4], resp[5]]) as usize;
    let ancount = u16::from_be_bytes([resp[6], resp[7]]) as usize;
    if qdcount != 1 || ancount < 1 {
        return None;
    }
    let mut idx = 12usize;
    // skip qname
    loop {
        if idx >= resp.len() {
            return None;
        }
        let len = resp[idx] as usize;
        idx += 1;
        if len == 0 {
            break;
        }
        idx += len;
    }
    if idx + 4 > resp.len() {
        return None;
    }
    idx += 4; // qtype/qclass

    // answer name: pointer or labels
    if idx + 2 > resp.len() {
        return None;
    }
    if resp[idx] & 0xc0 == 0xc0 {
        idx += 2;
    } else {
        loop {
            if idx >= resp.len() {
                return None;
            }
            let len = resp[idx] as usize;
            idx += 1;
            if len == 0 {
                break;
            }
            idx += len;
        }
    }
    if idx + 10 > resp.len() {
        return None;
    }
    let rr_type = u16::from_be_bytes([resp[idx], resp[idx + 1]]);
    let rr_class = u16::from_be_bytes([resp[idx + 2], resp[idx + 3]]);
    let rdlen = u16::from_be_bytes([resp[idx + 8], resp[idx + 9]]) as usize;
    idx += 10;
    if rr_type != 1 || rr_class != 1 || rdlen != 4 || idx + 4 > resp.len() {
        return None;
    }
    Some(Ipv4Addr::new(
        resp[idx],
        resp[idx + 1],
        resp[idx + 2],
        resp[idx + 3],
    ))
}

struct TcpEchoServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    active: Arc<AtomicUsize>,
}

impl TcpEchoServer {
    async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let active = Arc::new(AtomicUsize::new(0));
        let active_task = active.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let Ok((mut stream, _)) = accept else { continue };
                        active_task.fetch_add(1, Ordering::SeqCst);
                        let active_task = active_task.clone();
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 4096];
                            loop {
                                let n = match stream.read(&mut buf).await {
                                    Ok(0) => break,
                                    Ok(n) => n,
                                    Err(_) => break,
                                };
                                if stream.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                            active_task.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                }
            }
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            task,
            active,
        }
    }

    fn active_connections(&self) -> usize {
        self.active.load(Ordering::SeqCst)
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
    active: Arc<AtomicUsize>,
}

impl DohHttpServer {
    async fn spawn(records: HashMap<String, Ipv4Addr>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let active = Arc::new(AtomicUsize::new(0));
        let active_task = active.clone();
        let records = Arc::new(records);

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let Ok((stream, _)) = accept else { continue };
                        active_task.fetch_add(1, Ordering::SeqCst);
                        let active_task = active_task.clone();
                        let records = records.clone();
                        tokio::spawn(async move {
                            let _ = handle_http_conn(stream, &records).await;
                            active_task.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                }
            }
        });

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            task,
            active,
        }
    }

    fn active_connections(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

async fn handle_http_conn(
    mut stream: TcpStream,
    records: &HashMap<String, Ipv4Addr>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return Ok(());
        }
    }

    let req = String::from_utf8_lossy(&buf);
    let mut lines = req.lines();
    let Some(first) = lines.next() else {
        return Ok(());
    };
    let mut parts = first.split_whitespace();
    let Some(method) = parts.next() else {
        return Ok(());
    };
    let Some(path) = parts.next() else {
        return Ok(());
    };
    if method != "GET" {
        return Ok(());
    }

    let url = url::Url::parse(&format!("http://localhost{path}")).ok();
    let (status, content_type, body) = if let Some(url) = url {
        match url.path() {
            "/dns-query" => {
                let dns_param = url
                    .query_pairs()
                    .find(|(k, _)| k == "dns")
                    .map(|(_, v)| v.to_string());
                if let Some(dns_param) = dns_param {
                    let query = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(dns_param.as_bytes())
                        .ok();
                    if let Some(query) = query {
                        if let Some(q) = aero_net::protocol::parse_dns_query(&query) {
                            if let Some(ip) = records.get(&q.name) {
                                (
                                    200,
                                    "application/dns-message",
                                    build_dns_response_a(q.id, &q, *ip),
                                )
                            } else {
                                (
                                    200,
                                    "application/dns-message",
                                    build_dns_response_nxdomain(q.id, &q),
                                )
                            }
                        } else {
                            (400, "text/plain", b"bad dns".to_vec())
                        }
                    } else {
                        (400, "text/plain", b"bad b64".to_vec())
                    }
                } else {
                    (400, "text/plain", b"missing dns".to_vec())
                }
            }
            _ => (404, "text/plain", b"not found".to_vec()),
        }
    } else {
        (400, "text/plain", b"bad url".to_vec())
    };

    let resp = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}
