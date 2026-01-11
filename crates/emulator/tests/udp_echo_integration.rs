use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use aero_net_stack::packet::{EtherType, EthernetFrame, Ipv4Packet, Ipv4Protocol, MacAddr, UdpDatagram};
use emulator::io::net::stack::{Action, NetStackBackend, StackConfig, UdpProxyEvent, UdpTransport};

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

fn dhcp_handshake(backend: &mut NetStackBackend, cfg: &StackConfig, guest_mac: MacAddr) {
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
    backend.transmit_at(discover_frame, 0);
    backend.drain_frames();
    backend.drain_actions();

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
    backend.transmit_at(request_frame, 1);
    backend.drain_frames();
    backend.drain_actions();

    assert!(backend.stack().is_ip_assigned(), "DHCP should assign guest IP");
}

#[test]
fn udp_proxy_echo_end_to_end() {
    let server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let server_addr = server.local_addr().unwrap();
    let server_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let (n, peer) = match server.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => return,
        };
        let _ = server.send_to(&buf[..n], peer);
    });

    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let guest_mac = MacAddr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01]);

    let mut backend = NetStackBackend::new(cfg.clone());
    dhcp_handshake(&mut backend, &cfg, guest_mac);

    let remote_ip = Ipv4Addr::LOCALHOST;
    let remote_port = server_addr.port();
    let guest_port = 50000u16;
    let payload = b"hello over udp proxy";

    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        payload,
    );
    backend.transmit_at(frame, 10);

    let actions = backend.drain_actions();
    let [Action::UdpProxySend {
        transport,
        src_port,
        dst_ip,
        dst_port,
        data,
    }] = actions.as_slice()
    else {
        panic!("expected single UdpProxySend action, got {actions:?}");
    };
    assert_eq!(*transport, UdpTransport::WebRtc);
    assert_eq!(*src_port, guest_port);
    assert_eq!(*dst_ip, remote_ip);
    assert_eq!(*dst_port, remote_port);
    assert_eq!(data, payload);

    // Fulfill the proxy send by talking to the real UDP echo server.
    let proxy = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    proxy.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    proxy.send_to(data, server_addr).unwrap();
    let mut buf = [0u8; 2048];
    let (n, from) = proxy.recv_from(&mut buf).unwrap();
    assert_eq!(from, server_addr);
    let echoed = &buf[..n];

    backend.push_udp_event(
        UdpProxyEvent {
            src_ip: remote_ip,
            src_port: remote_port,
            dst_port: guest_port,
            data: echoed.to_vec(),
        },
        11,
    );

    let frames = backend.drain_frames();
    assert_eq!(frames.len(), 1);
    let eth = EthernetFrame::parse(&frames[0]).unwrap();
    assert_eq!(eth.ethertype, EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    assert_eq!(ip.protocol, Ipv4Protocol::UDP);
    assert_eq!(ip.src, remote_ip);
    assert_eq!(ip.dst, cfg.guest_ip);
    let udp = UdpDatagram::parse(ip.payload).unwrap();
    assert_eq!(udp.src_port, remote_port);
    assert_eq!(udp.dst_port, guest_port);
    assert_eq!(udp.payload, echoed);

    server_handle.join().unwrap();
}

