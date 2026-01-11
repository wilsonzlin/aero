use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use aero_net_stack::packet::{
    EtherType, EthernetFrame, Ipv4Packet, Ipv4Protocol, MacAddr, TcpFlags, TcpSegment, UdpDatagram,
};
use emulator::io::net::stack::{Action, StackConfig, TcpProxyEvent};
use emulator::io::net::trace::{CaptureArtifactOnPanic, NetTraceConfig, NetTracer, TracedNetworkStack};

fn wrap_udp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp = UdpDatagram::serialize(src_ip, dst_ip, src_port, dst_port, payload);
    let ip = Ipv4Packet::serialize(src_ip, dst_ip, Ipv4Protocol::UDP, 1, 64, &udp);
    EthernetFrame::serialize(dst_mac, src_mac, EtherType::IPV4, &ip)
}

fn wrap_tcp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
) -> Vec<u8> {
    let tcp = TcpSegment::serialize(src_ip, dst_ip, src_port, dst_port, seq, ack, flags, 65535, payload);
    let ip = Ipv4Packet::serialize(src_ip, dst_ip, Ipv4Protocol::TCP, 1, 64, &tcp);
    EthernetFrame::serialize(dst_mac, src_mac, EtherType::IPV4, &ip)
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

fn build_dhcp_request(xid: u32, mac: MacAddr, requested_ip: Ipv4Addr, server_id: Ipv4Addr) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
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

fn dhcp_handshake(stack: &mut TracedNetworkStack, cfg: &StackConfig, guest_mac: MacAddr) {
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
    stack.transmit_at(discover_frame, 0);
    stack.drain_frames();
    stack.drain_actions();

    let request = build_dhcp_request(xid, guest_mac, cfg.guest_ip, cfg.gateway_ip);
    let request_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &request,
    );
    stack.transmit_at(request_frame, 1);
    stack.drain_frames();
    stack.drain_actions();

    assert!(stack.inner().stack().is_ip_assigned(), "DHCP should assign guest IP");
}

fn read_exact_or_panic(stream: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).unwrap();
    buf
}

#[test]
fn tcp_proxy_echo_end_to_end() {
    // Local echo server (represents the host-side destination the proxy connects to).
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            stream.write_all(&buf[..n]).unwrap();
        }
    });

    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let guest_mac = MacAddr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    let remote_ip = Ipv4Addr::new(127, 0, 0, 1);

    let tracer = Arc::new(NetTracer::new(NetTraceConfig {
        capture_tcp_proxy: true,
        ..NetTraceConfig::default()
    }));
    tracer.enable();
    let _artifact = CaptureArtifactOnPanic::for_test(tracer.as_ref(), "tcp_proxy_echo_end_to_end");

    let mut stack = TracedNetworkStack::new(tracer.clone(), cfg.clone());
    dhcp_handshake(&mut stack, &cfg, guest_mac);

    let mut streams: HashMap<u32, TcpStream> = HashMap::new();

    let guest_port = 40000u16;
    let guest_isn = 1_000u32;

    // SYN from guest to remote.
    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        remote_ip,
        guest_port,
        addr.port(),
        guest_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    stack.transmit_at(syn, 20);

    let actions = stack.drain_actions();
    let frames = stack.drain_frames();

    let Action::TcpProxyConnect {
        connection_id,
        remote_ip: got_ip,
        remote_port: got_port,
    } = actions
        .iter()
        .find(|a| matches!(a, Action::TcpProxyConnect { .. }))
        .expect("expected TcpProxyConnect action")
    else {
        unreachable!();
    };
    assert_eq!(*got_ip, remote_ip);
    assert_eq!(*got_port, addr.port());

    assert_eq!(frames.len(), 1);
    let eth = EthernetFrame::parse(&frames[0]).unwrap();
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    let synack = TcpSegment::parse(ip.payload).unwrap();
    assert_eq!(synack.flags & (TcpFlags::SYN | TcpFlags::ACK), TcpFlags::SYN | TcpFlags::ACK);
    let stack_isn = synack.seq;

    // Host fulfills connect by opening a real socket, then notifies the stack.
    let stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    streams.insert(*connection_id, stream);
    stack.push_tcp_event(
        TcpProxyEvent::Connected {
            connection_id: *connection_id,
        },
        21,
    );
    assert!(stack.drain_actions().is_empty());

    // ACK to complete handshake.
    let ack = wrap_tcp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        remote_ip,
        guest_port,
        addr.port(),
        guest_isn + 1,
        stack_isn + 1,
        TcpFlags::ACK,
        &[],
    );
    stack.transmit_at(ack, 22);
    assert!(stack.drain_actions().is_empty());
    assert!(stack.drain_frames().is_empty());

    // Send data from guest; stack should emit a TcpProxySend action and an ACK frame.
    let payload = b"hello from guest";
    let psh = wrap_tcp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        remote_ip,
        guest_port,
        addr.port(),
        guest_isn + 1,
        stack_isn + 1,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    stack.transmit_at(psh, 23);
    let actions = stack.drain_actions();
    let frames = stack.drain_frames();
    assert!(
        frames.iter().any(|f| EthernetFrame::parse(f).is_ok()),
        "expected at least one ACK frame from stack"
    );

    let Action::TcpProxySend {
        connection_id: send_id,
        data,
    } = actions
        .iter()
        .find(|a| matches!(a, Action::TcpProxySend { .. }))
        .expect("expected TcpProxySend action")
    else {
        unreachable!();
    };
    assert_eq!(*send_id, *connection_id);
    assert_eq!(data, payload);

    // Fulfill TcpProxySend by writing to the real socket, then feed the echoed bytes back.
    let stream = streams.get_mut(connection_id).unwrap();
    stream.write_all(payload).unwrap();
    let echoed = read_exact_or_panic(stream, payload.len());

    stack.push_tcp_event(
        TcpProxyEvent::Data {
            connection_id: *connection_id,
            data: echoed.clone(),
        },
        24,
    );
    let frames = stack.drain_frames();
    assert_eq!(frames.len(), 1);
    let eth = EthernetFrame::parse(&frames[0]).unwrap();
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    let seg = TcpSegment::parse(ip.payload).unwrap();
    assert_eq!(seg.payload, echoed.as_slice());

    // ACK the echoed data.
    let guest_next = guest_isn + 1 + payload.len() as u32;
    let ack_remote = wrap_tcp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        remote_ip,
        guest_port,
        addr.port(),
        guest_next,
        seg.seq + seg.payload.len() as u32,
        TcpFlags::ACK,
        &[],
    );
    stack.transmit_at(ack_remote, 25);
    assert!(stack.drain_actions().is_empty());
    assert!(stack.drain_frames().is_empty());

    // FIN close from guest.
    let fin = wrap_tcp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        remote_ip,
        guest_port,
        addr.port(),
        guest_next,
        seg.seq + seg.payload.len() as u32,
        TcpFlags::ACK | TcpFlags::FIN,
        &[],
    );
    stack.transmit_at(fin, 26);
    let actions = stack.drain_actions();
    let frames = stack.drain_frames();

    assert!(actions.iter().any(|a| matches!(
        a,
        Action::TcpProxyClose { connection_id: id } if *id == *connection_id
    )));
    assert_eq!(frames.len(), 2, "expected ACK + FIN frames");

    // Close the host stream.
    streams.remove(connection_id);

    // ACK the stack FIN to let it drop state.
    let fin_seg = frames
        .iter()
        .find_map(|frame| {
            let eth = EthernetFrame::parse(frame).ok()?;
            let ip = Ipv4Packet::parse(eth.payload).ok()?;
            let seg = TcpSegment::parse(ip.payload).ok()?;
            (seg.flags & TcpFlags::FIN != 0).then_some(seg)
        })
        .expect("FIN from stack");

    let final_ack = wrap_tcp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        remote_ip,
        guest_port,
        addr.port(),
        guest_next + 1,
        fin_seg.seq + 1,
        TcpFlags::ACK,
        &[],
    );
    stack.transmit_at(final_ack, 27);
    assert!(stack.drain_actions().is_empty());
    assert!(stack.drain_frames().is_empty());

    let capture = tracer.export_pcapng();
    assert!(
        capture.windows(4).any(|w| w == b"ATCP"),
        "expected traced capture to include tcp-proxy packets"
    );

    server_handle.join().unwrap();
}

