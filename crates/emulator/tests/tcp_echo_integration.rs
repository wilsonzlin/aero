use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use emulator::io::net::stack::dns::{DnsAnswer, DnsUpstream};
use emulator::io::net::stack::ethernet::{build_ethernet_frame, EtherType, EthernetFrame, MacAddr};
use emulator::io::net::stack::ipv4::{checksum_pseudo_header, finalize_checksum, IpProtocol, Ipv4Packet, Ipv4PacketBuilder};
use emulator::io::net::stack::tcp_nat::TcpSegment;
use emulator::io::net::stack::{NetConfig, NetworkStack, ProxyAction, ProxyEvent};

struct NoDns;

impl DnsUpstream for NoDns {
    fn resolve_a(&mut self, _name: &str) -> Option<DnsAnswer> {
        None
    }
}

fn build_tcp_segment(
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
) -> Vec<u8> {
    let header_len = 20usize;
    let total_len = header_len + payload.len();
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&src_port.to_be_bytes());
    out.extend_from_slice(&dst_port.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ack.to_be_bytes());
    out.push((5u8 << 4) | 0);
    out.push(flags as u8);
    out.extend_from_slice(&65535u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out.extend_from_slice(&0u16.to_be_bytes()); // urgent ptr
    out.extend_from_slice(payload);

    let mut sum = checksum_pseudo_header(src_ip, dst_ip, IpProtocol::Tcp.to_u8(), total_len as u16);
    for chunk in out.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if out.len() % 2 == 1 {
        sum += (out[out.len() - 1] as u32) << 8;
    }
    let csum = finalize_checksum(sum);
    out[16..18].copy_from_slice(&csum.to_be_bytes());
    out
}

fn build_guest_frame(
    cfg: &NetConfig,
    guest_mac: MacAddr,
    dst_ip: Ipv4Addr,
    tcp: Vec<u8>,
) -> Vec<u8> {
    let ip = Ipv4PacketBuilder::new()
        .src(cfg.guest_ip)
        .dst(dst_ip)
        .protocol(IpProtocol::Tcp)
        .payload(tcp)
        .build();
    build_ethernet_frame(cfg.gateway_mac, guest_mac, EtherType::Ipv4, &ip)
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

    let cfg = NetConfig::default();
    let guest_mac = MacAddr::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    let remote_ip = Ipv4Addr::new(127, 0, 0, 1);

    let mut stack = NetworkStack::new(cfg.clone(), NoDns);
    let mut streams: HashMap<u64, TcpStream> = HashMap::new();

    let guest_port = 40000u16;
    let guest_isn = 1_000u32;

    // SYN from guest to remote.
    let syn = build_tcp_segment(guest_port, addr.port(), guest_isn, 0, 0x02, &[], cfg.guest_ip, remote_ip);
    let syn_frame = build_guest_frame(&cfg, guest_mac, remote_ip, syn);
    let out = stack.process_frame_from_guest(&syn_frame);
    assert_eq!(out.proxy_actions.len(), 1);
    assert_eq!(out.frames_to_guest.len(), 1);

    let ProxyAction::TcpConnect { conn_id, dst_ip, dst_port } = out.proxy_actions[0] else {
        panic!("expected TcpConnect");
    };
    assert_eq!(dst_ip, remote_ip);
    assert_eq!(dst_port, addr.port());

    let stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    streams.insert(conn_id, stream);

    // Parse SYN-ACK to learn stack ISN.
    let eth = EthernetFrame::parse(&out.frames_to_guest[0]).unwrap();
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    let synack = TcpSegment::parse(ip.payload).unwrap();
    assert_eq!(synack.flags & 0x12, 0x12); // SYN|ACK
    let stack_isn = synack.seq;

    // ACK to complete handshake.
    let ack = build_tcp_segment(
        guest_port,
        addr.port(),
        guest_isn + 1,
        stack_isn + 1,
        0x10,
        &[],
        cfg.guest_ip,
        remote_ip,
    );
    let ack_frame = build_guest_frame(&cfg, guest_mac, remote_ip, ack);
    let out = stack.process_frame_from_guest(&ack_frame);
    assert!(out.proxy_actions.is_empty());

    // Send data from guest; stack should emit a TcpSend action.
    let payload = b"hello from guest";
    let data = build_tcp_segment(
        guest_port,
        addr.port(),
        guest_isn + 1,
        stack_isn + 1,
        0x18, // PSH|ACK
        payload,
        cfg.guest_ip,
        remote_ip,
    );
    let data_frame = build_guest_frame(&cfg, guest_mac, remote_ip, data);
    let out = stack.process_frame_from_guest(&data_frame);
    assert_eq!(out.proxy_actions.len(), 1);
    assert!(matches!(out.proxy_actions[0], ProxyAction::TcpSend { .. }));

    let ProxyAction::TcpSend { conn_id: send_conn, data } = &out.proxy_actions[0] else {
        unreachable!()
    };
    assert_eq!(*send_conn, conn_id);
    assert_eq!(data, payload);

    // Fulfill TcpSend by writing to the real socket, then feed the echoed bytes back via ProxyEvent.
    let stream = streams.get_mut(&conn_id).unwrap();
    stream.write_all(payload).unwrap();
    let echoed = read_exact_or_panic(stream, payload.len());

    let out = stack.process_proxy_event(ProxyEvent::TcpData { conn_id, data: echoed.clone() });
    assert_eq!(out.frames_to_guest.len(), 1);
    let eth = EthernetFrame::parse(&out.frames_to_guest[0]).unwrap();
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    let seg = TcpSegment::parse(ip.payload).unwrap();
    assert_eq!(seg.payload, echoed.as_slice());

    // ACK the echoed data.
    let ack_remote = build_tcp_segment(
        guest_port,
        addr.port(),
        guest_isn + 1 + payload.len() as u32,
        seg.seq + seg.payload.len() as u32,
        0x10,
        &[],
        cfg.guest_ip,
        remote_ip,
    );
    let ack_remote_frame = build_guest_frame(&cfg, guest_mac, remote_ip, ack_remote);
    let out = stack.process_frame_from_guest(&ack_remote_frame);
    assert!(out.proxy_actions.is_empty());

    // FIN close from guest.
    let fin = build_tcp_segment(
        guest_port,
        addr.port(),
        guest_isn + 1 + payload.len() as u32,
        seg.seq + seg.payload.len() as u32,
        0x11, // FIN|ACK
        &[],
        cfg.guest_ip,
        remote_ip,
    );
    let fin_frame = build_guest_frame(&cfg, guest_mac, remote_ip, fin);
    let out = stack.process_frame_from_guest(&fin_frame);
    assert_eq!(out.proxy_actions.len(), 1);
    assert!(matches!(out.proxy_actions[0], ProxyAction::TcpClose { .. }));

    // Close the host stream.
    streams.remove(&conn_id);

    // Stack sends ACK + FIN; ACK the FIN to let it drop state.
    assert_eq!(out.frames_to_guest.len(), 2);
    let fin_seg = out
        .frames_to_guest
        .iter()
        .find_map(|frame| {
            let eth = EthernetFrame::parse(frame)?;
            let ip = Ipv4Packet::parse(eth.payload)?;
            let seg = TcpSegment::parse(ip.payload)?;
            if (seg.flags & 0x01) != 0 {
                Some(seg)
            } else {
                None
            }
        })
        .expect("FIN from stack");

    let final_ack = build_tcp_segment(
        guest_port,
        addr.port(),
        guest_isn + 1 + payload.len() as u32 + 1,
        fin_seg.seq + 1,
        0x10,
        &[],
        cfg.guest_ip,
        remote_ip,
    );
    let final_ack_frame = build_guest_frame(&cfg, guest_mac, remote_ip, final_ack);
    let out = stack.process_frame_from_guest(&final_ack_frame);
    assert!(out.frames_to_guest.is_empty());

    server_handle.join().unwrap();
}
