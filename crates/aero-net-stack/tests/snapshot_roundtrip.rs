use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_net_stack::packet::*;
use aero_net_stack::{Action, DnsResolved, NetworkStack, StackConfig, TcpProxyEvent, UdpProxyEvent};
use core::net::Ipv4Addr;

#[test]
fn snapshot_roundtrip_preserves_dns_cache_ttl_and_drops_tcp_state() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let mut stack = NetworkStack::new(cfg.clone());

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);

    // Create at least one TCP connection before snapshot. (This will be dropped on restore.)
    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let guest_port = 40000;
    let guest_isn = 12345;
    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn, 100);
    let (conn_id, syn_ack_frame) = extract_tcp_connect_and_frame(&actions);
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame);

    let ack = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn + 1,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK,
        &[],
    );
    assert!(
        stack.process_outbound_ethernet(&ack, 101).is_empty(),
        "TCP handshake ACK should not produce actions"
    );

    // Insert a DNS cache entry with a short TTL.
    let dns_query = build_dns_query(0x1234, "example.com", DnsType::A as u16);
    let dns_frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        53000,
        53,
        &dns_query,
    );
    let actions = stack.process_outbound_ethernet(&dns_frame, 1000);
    let (dns_req_id, name) = match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        _ => panic!("expected single DnsResolve action, got {actions:?}"),
    };

    let _dns_resp = stack.handle_dns_resolved(
        DnsResolved {
            request_id: dns_req_id,
            name,
            addr: Some(Ipv4Addr::new(1, 2, 3, 4)),
            ttl_secs: 2,
        },
        1000,
    );

    // Cache hit before snapshot: should include ~1 second remaining.
    let actions = stack.process_outbound_ethernet(&dns_frame, 1500);
    let cached = extract_single_frame(&actions);
    assert_eq!(dns_answer_ttl_secs(&cached), 1);

    // Snapshot the stack at internal now_ms = 1500.
    let snap = stack.save_state();

    // Restore into a new stack where the host time base restarts from 0.
    let mut restored = NetworkStack::new(cfg);
    restored.load_state(&snap).unwrap();
    assert!(restored.is_ip_assigned());

    // Prove guest MAC + IP-assigned state survived restore (without relying on an outbound frame to
    // re-learn the MAC).
    let udp_actions = restored.handle_udp_proxy_event(
        UdpProxyEvent {
            src_ip: remote_ip,
            src_port: 9999,
            dst_port: 50000,
            data: b"hi".to_vec(),
        },
        0,
    );
    assert!(
        udp_actions.iter().any(|a| matches!(a, Action::EmitFrame(_))),
        "expected UDP proxy event to produce a guest frame after restore"
    );

    // Cache hit after restore at now_ms=0 should preserve the remaining TTL (not reset it).
    let actions = restored.process_outbound_ethernet(&dns_frame, 0);
    let cached = extract_single_frame(&actions);
    assert_eq!(dns_answer_ttl_secs(&cached), 1);

    // After enough time passes, cache entry should be treated as expired (trigger resolve).
    let actions = restored.process_outbound_ethernet(&dns_frame, 1600);
    assert!(
        matches!(actions.as_slice(), [Action::DnsResolve { .. }]),
        "expected DNS resolve after TTL expiry across restore, got {actions:?}"
    );

    // Restore policy: TCP state is dropped. Proxy data for the pre-snapshot connection must be
    // ignored.
    let actions = restored.handle_tcp_proxy_event(
        TcpProxyEvent::Data {
            connection_id: conn_id,
            data: b"late".to_vec(),
        },
        0,
    );
    assert!(actions.is_empty());

    // Connection IDs should not be reused after restore; the next connection should use the next
    // ID.
    let syn2 = wrap_tcp_ipv4_eth(
        guest_mac,
        restored.config().our_mac,
        restored.config().guest_ip,
        remote_ip,
        guest_port + 1,
        80,
        99999,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = restored.process_outbound_ethernet(&syn2, 10);
    let (new_conn_id, _) = extract_tcp_connect_and_frame(&actions);
    assert_eq!(new_conn_id, conn_id + 1);
}

#[test]
fn snapshot_rejects_excessive_dns_cache_entry_count() {
    // Tag numbers are part of the snapshot format documented in `aero-net-stack/src/stack.rs`.
    const TAG_DNS_CACHE: u16 = 6;

    let mut w = SnapshotWriter::new(
        <NetworkStack as IoSnapshot>::DEVICE_ID,
        <NetworkStack as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_bytes(TAG_DNS_CACHE, u32::MAX.to_le_bytes().to_vec());

    let mut stack = NetworkStack::new(StackConfig::default());
    let err = stack
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive DNS cache entry count");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("too many dns cache entries"));
}

#[test]
fn snapshot_loads_legacy_device_id_header() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let mut stack = NetworkStack::new(cfg.clone());

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);

    let bytes = stack.save_state();
    assert_eq!(&bytes[8..12], b"NETS");

    // Older snapshots used an accidental device id for the network stack blob. Ensure we can still
    // decode them.
    const LEGACY_DEVICE_ID: [u8; 4] = [0x4e, 0x53, 0x54, 0x4b];
    let mut legacy = bytes.clone();
    legacy[8..12].copy_from_slice(&LEGACY_DEVICE_ID);

    let mut restored = NetworkStack::new(cfg);
    restored
        .load_state(&legacy)
        .expect("legacy snapshot should decode");
    assert!(restored.is_ip_assigned());

    // Re-saving always uses the canonical device id.
    let resaved = restored.save_state();
    assert_eq!(&resaved[8..12], b"NETS");
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

    // Offer.
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

    // Ack.
    let _ = stack.process_outbound_ethernet(&request_frame, 1);

    assert!(stack.is_ip_assigned());
}

fn extract_single_frame(actions: &[Action]) -> Vec<u8> {
    let frames: Vec<Vec<u8>> = actions
        .iter()
        .filter_map(|a| match a {
            Action::EmitFrame(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(frames.len(), 1, "expected 1 EmitFrame, got {actions:?}");
    frames.into_iter().next().unwrap()
}

fn extract_tcp_connect_and_frame(actions: &[Action]) -> (u32, Vec<u8>) {
    let mut conn_id = None;
    let mut frame = None;
    for a in actions {
        match a {
            Action::TcpProxyConnect {
                connection_id,
                remote_ip: _,
                remote_port: _,
            } => conn_id = Some(*connection_id),
            Action::EmitFrame(f) => frame = Some(f.clone()),
            _ => {}
        }
    }
    (
        conn_id.expect("missing TcpProxyConnect"),
        frame.expect("missing EmitFrame"),
    )
}

fn parse_tcp_from_frame(frame: &[u8]) -> TcpSegment<'_> {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    assert_eq!(ip.protocol(), Ipv4Protocol::TCP);
    TcpSegment::parse(ip.payload()).unwrap()
}

fn dns_answer_ttl_secs(frame: &[u8]) -> u32 {
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    let udp = UdpPacket::parse(ip.payload()).unwrap();
    let dns = udp.payload();
    let ttl = &dns[dns.len() - 10..dns.len() - 6];
    u32::from_be_bytes([ttl[0], ttl[1], ttl[2], ttl[3]])
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
