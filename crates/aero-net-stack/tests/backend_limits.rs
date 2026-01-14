use aero_net_backend::NetworkBackend;
use aero_net_stack::packet::*;
use aero_net_stack::{Action, NetStackBackend, NetStackBackendLimits, StackConfig, UdpTransport};
use core::net::Ipv4Addr;

#[test]
fn pending_frames_is_bounded_and_poll_receive_is_fifo() {
    let limits = NetStackBackendLimits {
        max_pending_frames: 2,
        max_pending_actions: 16,
        max_pending_action_bytes: 1024,
    };
    let mut backend = NetStackBackend::with_limits(StackConfig::default(), limits);

    let cfg = backend.stack().config().clone();

    let mut reqs = Vec::new();
    for i in 0..5u8 {
        let sender_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, i]);
        let sender_ip = Ipv4Addr::new(10, 0, 2, 100 + i);
        let arp = ArpPacketBuilder {
            opcode: ARP_OP_REQUEST,
            sender_mac,
            sender_ip,
            target_mac: MacAddr([0u8; 6]),
            target_ip: cfg.gateway_ip,
        }
        .build_vec()
        .unwrap();
        let frame = EthernetFrameBuilder {
            dest_mac: MacAddr::BROADCAST,
            src_mac: sender_mac,
            ethertype: EtherType::ARP,
            payload: &arp,
        }
        .build_vec()
        .unwrap();
        backend.transmit_at(frame, i as u64);
        reqs.push((sender_mac, sender_ip));
    }

    let stats = backend.stats();
    assert_eq!(stats.pending_frames, 2);
    assert_eq!(stats.dropped_frames, 3);

    // Ensure we kept the *oldest* two frames and did not evict.
    for (expected_mac, expected_ip) in reqs.into_iter().take(2) {
        let frame = backend.poll_receive().expect("expected queued frame");
        let eth = EthernetFrame::parse(&frame).unwrap();
        assert_eq!(eth.ethertype(), EtherType::ARP);
        let arp = ArpPacket::parse(eth.payload()).unwrap();
        assert_eq!(arp.opcode(), ARP_OP_REPLY);
        assert_eq!(arp.sender_mac(), Some(cfg.our_mac));
        assert_eq!(arp.sender_ip(), Some(cfg.gateway_ip));
        assert_eq!(arp.target_mac(), Some(expected_mac));
        assert_eq!(arp.target_ip(), Some(expected_ip));
    }
    assert!(backend.poll_receive().is_none());
}

#[test]
fn pending_actions_is_bounded_and_drain_actions_is_fifo() {
    let limits = NetStackBackendLimits {
        max_pending_frames: 16,
        max_pending_actions: 2,
        max_pending_action_bytes: 1024,
    };
    let mut backend = NetStackBackend::with_limits(StackConfig::default(), limits);
    backend.stack_mut().set_network_enabled(true);

    let cfg = backend.stack().config().clone();
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);

    for i in 0..5u16 {
        let name = format!("q{i}.example");
        let query = build_dns_query(0x1000 + i, &name, DnsType::A as u16);
        let frame = wrap_udp_ipv4_eth(
            guest_mac,
            cfg.our_mac,
            cfg.guest_ip,
            cfg.dns_ip,
            53000 + i,
            53,
            &query,
        );
        backend.transmit_at(frame, i as u64);
    }

    let stats = backend.stats();
    assert_eq!(stats.pending_actions, 2);
    assert_eq!(stats.dropped_actions, 3);

    let actions = backend.drain_actions();
    assert_eq!(actions.len(), 2);
    assert!(matches!(
        actions[0],
        Action::DnsResolve { request_id: 1, .. }
    ));
    assert!(matches!(
        actions[1],
        Action::DnsResolve { request_id: 2, .. }
    ));

    let stats = backend.stats();
    assert_eq!(stats.pending_actions, 0);
    assert_eq!(stats.pending_action_bytes, 0);
}

#[test]
fn pending_action_bytes_is_bounded_for_udp_proxy_send_payloads() {
    let limits = NetStackBackendLimits {
        max_pending_frames: 64,
        max_pending_actions: 16,
        max_pending_action_bytes: 4,
    };
    let mut backend = NetStackBackend::with_limits(StackConfig::default(), limits);

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut backend, guest_mac);
    backend.drain_frames();
    backend.stack_mut().set_network_enabled(true);

    let cfg = backend.stack().config().clone();
    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);

    // Three packets of size 3 bytes each. Only the first fits in the 4-byte cap.
    for i in 0..3u16 {
        let payload = vec![0x42u8; 3];
        let frame = wrap_udp_ipv4_eth(
            guest_mac,
            cfg.our_mac,
            cfg.guest_ip,
            remote_ip,
            50000 + i,
            9999,
            &payload,
        );
        backend.transmit_at(frame, i as u64);
    }

    let stats = backend.stats();
    assert_eq!(stats.pending_actions, 1);
    assert_eq!(stats.pending_action_bytes, 3);
    assert_eq!(stats.dropped_actions, 2);
    assert_eq!(stats.dropped_action_bytes, 6);

    let actions = backend.drain_actions();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::UdpProxySend {
            transport,
            dst_ip,
            dst_port,
            data,
            ..
        } => {
            assert_eq!(*transport, UdpTransport::WebRtc);
            assert_eq!(*dst_ip, remote_ip);
            assert_eq!(*dst_port, 9999);
            assert_eq!(data.len(), 3);
        }
        other => panic!("expected UdpProxySend, got {other:?}"),
    }

    let stats = backend.stats();
    assert_eq!(stats.pending_actions, 0);
    assert_eq!(stats.pending_action_bytes, 0);
}

fn dhcp_handshake(backend: &mut NetStackBackend, guest_mac: MacAddr) {
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

    let request = build_dhcp_request(
        xid,
        guest_mac,
        backend.stack().config().guest_ip,
        backend.stack().config().gateway_ip,
    );
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

    assert!(backend.stack().is_ip_assigned());
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

fn build_dhcp_request(
    xid: u32,
    mac: MacAddr,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
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

fn build_dns_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // IN
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
    .unwrap();
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
    .unwrap();
    EthernetFrameBuilder {
        dest_mac: dst_mac,
        src_mac,
        ethertype: EtherType::IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap()
}
