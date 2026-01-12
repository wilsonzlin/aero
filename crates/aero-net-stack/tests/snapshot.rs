use aero_io_snapshot::io::state::IoSnapshot;
use aero_net_stack::packet::*;
use aero_net_stack::{
    Action, DnsResolved, NetworkStack, NetworkStackSnapshotState, StackConfig, TcpRestorePolicy,
};
use core::net::Ipv4Addr;

#[test]
fn snapshot_roundtrip_preserves_dhcp_and_dns_cache() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);

    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    // Populate DNS cache with one entry.
    let query = build_dns_query(0x1234, "example.com", DnsType::A as u16);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        53000,
        53,
        &query,
    );
    let actions = stack.process_outbound_ethernet(&frame, 10);
    let (request_id, name) = match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        other => panic!("expected DnsResolve action, got {other:?}"),
    };
    assert_eq!(name, "example.com");

    let _ = stack.handle_dns_resolved(
        DnsResolved {
            request_id,
            name,
            addr: Some(Ipv4Addr::new(93, 184, 216, 34)),
            ttl_secs: 60,
        },
        11,
    );

    let state_a = stack.export_snapshot_state();
    let bytes = state_a.save_state();

    // `aero-io-snapshot` header + our device id.
    assert!(bytes.len() >= 16);
    assert_eq!(&bytes[0..4], b"AERO");
    assert_eq!(&bytes[8..12], b"NETS");

    let mut decoded = NetworkStackSnapshotState::default();
    decoded.load_state(&bytes).expect("decode snapshot");

    let mut restored = NetworkStack::new(StackConfig::default());
    restored.import_snapshot_state(decoded, TcpRestorePolicy::Drop);
    let state_b = restored.export_snapshot_state();

    assert_eq!(state_a.guest_mac, state_b.guest_mac);
    assert_eq!(state_a.ip_assigned, state_b.ip_assigned);
    assert_eq!(state_a.next_tcp_id, state_b.next_tcp_id);
    assert_eq!(state_a.next_dns_id, state_b.next_dns_id);
    assert_eq!(state_a.ipv4_ident, state_b.ipv4_ident);
    assert_eq!(state_a.last_now_ms, state_b.last_now_ms);
    assert_eq!(state_a.dns_cache, state_b.dns_cache);
}

#[test]
fn snapshot_drop_policy_clears_tcp_state() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    // Create a single TCP connection.
    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        Ipv4Addr::new(93, 184, 216, 34),
        40000,
        80,
        1000,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn, 20);
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::TcpProxyConnect { .. })));

    let state_a = stack.export_snapshot_state();
    assert_eq!(state_a.tcp_connections.len(), 1);

    let bytes = state_a.save_state();
    let mut decoded = NetworkStackSnapshotState::default();
    decoded.load_state(&bytes).expect("decode snapshot");

    let mut restored = NetworkStack::new(StackConfig::default());
    restored.import_snapshot_state(decoded, TcpRestorePolicy::Drop);
    let state_b = restored.export_snapshot_state();

    assert!(
        state_b.tcp_connections.is_empty(),
        "Drop policy should clear TCP state"
    );
    assert_eq!(
        state_b.next_tcp_id, state_a.next_tcp_id,
        "ID allocator should be preserved even when dropping TCP conns"
    );
}

#[test]
fn snapshot_corrupt_bytes_returns_error() {
    let mut state = NetworkStackSnapshotState::default();
    assert!(state.load_state(&[]).is_err());
    assert!(state.load_state(&[0u8; 16]).is_err());
}

fn dhcp_handshake(stack: &mut NetworkStack, guest_mac: MacAddr) {
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
    let _ = stack.process_outbound_ethernet(&discover_frame, 0);

    let request = build_dhcp_request(
        xid,
        guest_mac,
        stack.config().guest_ip,
        stack.config().gateway_ip,
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
    let _ = stack.process_outbound_ethernet(&request_frame, 1);
    assert!(stack.is_ip_assigned());
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
    out.extend_from_slice(&1u16.to_be_bytes());
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

#[allow(clippy::too_many_arguments)]
fn wrap_tcp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: TcpFlags,
    payload: &[u8],
) -> Vec<u8> {
    let tcp = TcpSegmentBuilder {
        src_port,
        dst_port,
        seq_number: seq,
        ack_number: ack,
        flags,
        window_size: 65535,
        urgent_pointer: 0,
        options: &[],
        payload,
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();
    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0x4000, // DF
        ttl: 64,
        protocol: Ipv4Protocol::TCP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &tcp,
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
