use aero_net_stack::packet::*;
use aero_net_stack::{
    Action, DnsCacheEntrySnapshot, DnsResolved, IpCidr, NetworkStack, NetworkStackSnapshotState,
    StackConfig, TcpProxyEvent, TcpRestorePolicy,
};
use core::net::Ipv4Addr;

#[test]
fn dhcp_dns_tcp_flow() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);

    // --- DHCP handshake ---
    dhcp_handshake(&mut stack, guest_mac);

    // --- ARP for gateway ---
    let arp_req = ArpPacketBuilder {
        opcode: ARP_OP_REQUEST,
        sender_mac: guest_mac,
        sender_ip: stack.config().guest_ip,
        target_mac: MacAddr([0u8; 6]),
        target_ip: stack.config().gateway_ip,
    }
    .build_vec()
    .expect("build ARP request");
    let frame = EthernetFrameBuilder {
        dest_mac: MacAddr::BROADCAST,
        src_mac: guest_mac,
        ethertype: EtherType::ARP,
        payload: &arp_req,
    }
    .build_vec()
    .expect("build Ethernet frame");
    let actions = stack.process_outbound_ethernet(&frame, 1);
    let arp_resp_frame = extract_single_frame(&actions);
    let eth = EthernetFrame::parse(&arp_resp_frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::ARP);
    assert_eq!(eth.src_mac(), stack.config().our_mac);
    assert_eq!(eth.dest_mac(), guest_mac);
    let arp = ArpPacket::parse(eth.payload()).unwrap();
    assert_eq!(arp.opcode(), ARP_OP_REPLY);
    assert_eq!(arp.sender_mac().unwrap(), stack.config().our_mac);
    assert_eq!(arp.sender_ip().unwrap(), stack.config().gateway_ip);
    assert_eq!(arp.target_mac().unwrap(), guest_mac);
    assert_eq!(arp.target_ip().unwrap(), stack.config().guest_ip);

    // Networking is default-deny until enabled.
    let syn_denied = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        Ipv4Addr::new(93, 184, 216, 34),
        40000,
        80,
        100,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn_denied, 2);
    assert!(actions
        .iter()
        .all(|a| !matches!(a, Action::TcpProxyConnect { .. })));
    assert!(actions.iter().any(|a| matches!(a, Action::EmitFrame(_))));

    stack.set_network_enabled(true);

    // --- DNS lookup ---
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
    let actions = stack.process_outbound_ethernet(&dns_frame, 10);
    let (dns_req_id, name) = match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        _ => panic!("expected single DnsResolve action, got {actions:?}"),
    };
    assert_eq!(name, "example.com");

    let dns_actions = stack.handle_dns_resolved(
        DnsResolved {
            request_id: dns_req_id,
            name,
            addr: Some(Ipv4Addr::new(93, 184, 216, 34)),
            ttl_secs: 60,
        },
        11,
    );
    let dns_resp_frame = extract_single_frame(&dns_actions);
    assert_dns_response_has_a_record(&dns_resp_frame, 0x1234, [93, 184, 216, 34]);

    // --- TCP connect + data ---
    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let guest_port = 40001;
    let guest_isn = 5000;
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
    let actions = stack.process_outbound_ethernet(&syn, 20);
    let (conn_id, syn_ack_frame) = extract_tcp_connect_and_frame(&actions);
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame);
    assert_eq!(syn_ack.flags(), TcpFlags::SYN | TcpFlags::ACK);
    assert_eq!(syn_ack.ack_number(), guest_isn + 1);

    // Proxy connects.
    let proxy_actions = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Connected {
            connection_id: conn_id,
        },
        21,
    );
    assert!(proxy_actions.is_empty());

    // Complete handshake.
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
    let actions = stack.process_outbound_ethernet(&ack, 22);
    assert!(actions.is_empty());

    // Send application data.
    let payload = b"GET / HTTP/1.0\r\n\r\n";
    let psh = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn + 1,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    let actions = stack.process_outbound_ethernet(&psh, 23);
    assert!(actions.iter().any(|a| matches!(a, Action::TcpProxySend { connection_id, data } if *connection_id == conn_id && data == payload)));
    assert!(actions.iter().any(|a| matches!(a, Action::EmitFrame(_))));

    // Receive data back from proxy.
    let resp_payload = b"HTTP/1.0 200 OK\r\n\r\n";
    let actions = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Data {
            connection_id: conn_id,
            data: resp_payload.to_vec(),
        },
        24,
    );
    let frame = extract_single_frame(&actions);
    let seg = parse_tcp_from_frame(&frame);
    assert_eq!(seg.payload(), resp_payload);
    assert_eq!(seg.flags(), TcpFlags::ACK | TcpFlags::PSH);
}

#[test]
fn tcp_invalid_ack_does_not_complete_handshake() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let guest_port = 40100;
    let guest_isn = 9000;

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
    let actions = stack.process_outbound_ethernet(&syn, 0);
    let (conn_id, syn_ack_frame) = extract_tcp_connect_and_frame(&actions);
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame);

    // Send an ACK that does not acknowledge the stack's SYN (ack_number < our_isn).
    let bad_ack = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn + 1,
        0,
        TcpFlags::ACK,
        &[],
    );
    assert!(stack.process_outbound_ethernet(&bad_ack, 1).is_empty());

    // If the proxy side closes before the handshake is actually complete, the guest should see an
    // RST (not a FIN).
    let actions = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Closed {
            connection_id: conn_id,
        },
        2,
    );
    let frame = extract_single_frame(&actions);
    let seg = parse_tcp_from_frame(&frame);
    assert!(
        seg.flags().contains(TcpFlags::RST),
        "expected RST, got {:?}",
        seg.flags()
    );
    assert!(
        !seg.flags().contains(TcpFlags::FIN),
        "unexpected FIN in {:?}",
        seg.flags()
    );

    // Sanity-check: the earlier SYN-ACK we sent is a SYN|ACK (a regression guard for the setup).
    assert_eq!(syn_ack.flags(), TcpFlags::SYN | TcpFlags::ACK);
}

#[test]
fn dns_aaaa_query_returns_notimp() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    let query = build_dns_query(0x2222, "example.com", 28 /* AAAA */);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        53001,
        53,
        &query,
    );
    let actions = stack.process_outbound_ethernet(&frame, 0);
    assert!(actions
        .iter()
        .all(|a| !matches!(a, Action::DnsResolve { .. })));

    let resp_frame = extract_single_frame(&actions);
    assert_dns_response_has_rcode(&resp_frame, 0x2222, DnsResponseCode::NotImplemented);
}

#[test]
fn dns_cache_ttl_is_clamped_to_u32_max() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let mut stack = NetworkStack::new(cfg);

    // Snapshots can contain arbitrary `expires_at_ms` values. Ensure the synthesized DNS TTL does
    // not truncate when the remaining lifetime exceeds the u32 TTL field.
    let snapshot = NetworkStackSnapshotState {
        guest_mac: None,
        ip_assigned: true,
        next_tcp_id: 1,
        next_dns_id: 1,
        ipv4_ident: 1,
        last_now_ms: 0,
        dns_cache: vec![DnsCacheEntrySnapshot {
            name: "example.com".to_string(),
            addr: Ipv4Addr::new(93, 184, 216, 34),
            expires_at_ms: u64::MAX,
        }],
        tcp_connections: Vec::new(),
    };
    let _ = stack.import_snapshot_state(snapshot, TcpRestorePolicy::Drop);

    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    let query = build_dns_query(0x5555, "example.com", DnsType::A as u16);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        53005,
        53,
        &query,
    );
    let actions = stack.process_outbound_ethernet(&frame, 0);
    let resp_frame = extract_single_frame(&actions);

    let udp = parse_udp_from_frame(&resp_frame);
    let dns_payload = udp.payload();
    let question = dns::parse_single_question(dns_payload).expect("parse_single_question");
    let question_len = 12 + question.qname.len() + 4;
    let ttl_off = question_len + 2 + 2 + 2;
    let ttl = u32::from_be_bytes(
        dns_payload[ttl_off..ttl_off + 4]
            .try_into()
            .expect("ttl bytes"),
    );
    assert_eq!(ttl, u32::MAX);
}

#[test]
fn tcp_fin_closes_and_drops_state() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let guest_port = 40010;
    let guest_isn = 12345;

    // SYN.
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
    let actions = stack.process_outbound_ethernet(&syn, 0);
    let (conn_id, syn_ack_frame) = extract_tcp_connect_and_frame(&actions);
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame);

    // Proxy connects and guest completes handshake.
    assert!(stack
        .handle_tcp_proxy_event(
            TcpProxyEvent::Connected {
                connection_id: conn_id
            },
            1
        )
        .is_empty());
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
    assert!(stack.process_outbound_ethernet(&ack, 2).is_empty());

    // One payload chunk.
    let payload = b"hello";
    let psh = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn + 1,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    let actions = stack.process_outbound_ethernet(&psh, 3);
    assert!(actions.iter().any(
        |a| matches!(a, Action::TcpProxySend { connection_id, data } if *connection_id == conn_id && data == payload)
    ));

    // FIN from guest.
    let guest_next = guest_isn + 1 + payload.len() as u32;
    let fin = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_next,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK | TcpFlags::FIN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&fin, 4);
    assert!(actions.iter().any(
        |a| matches!(a, Action::TcpProxyClose { connection_id } if *connection_id == conn_id)
    ));

    let frames = extract_frames(&actions);
    assert_eq!(frames.len(), 2, "expected ACK + FIN frames");
    let fin_seg = frames
        .iter()
        .map(|f| parse_tcp_from_frame(f))
        .find(|seg| seg.flags().contains(TcpFlags::FIN))
        .expect("FIN from stack");

    // ACK the stack FIN; stack should drop state, so no further frames/actions.
    let final_ack = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_next + 1,
        fin_seg.seq_number() + 1,
        TcpFlags::ACK,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&final_ack, 5);
    assert!(actions.is_empty());

    // Late proxy data should be ignored after state is dropped.
    let late = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Data {
            connection_id: conn_id,
            data: b"late".to_vec(),
        },
        6,
    );
    assert!(late.is_empty());
}

#[test]
fn tcp_proxy_error_before_handshake_sends_rst() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let guest_port = 40100;
    let guest_isn = 9000;

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
    let actions = stack.process_outbound_ethernet(&syn, 0);
    let (conn_id, _syn_ack_frame) = extract_tcp_connect_and_frame(&actions);

    let actions = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Error {
            connection_id: conn_id,
        },
        1,
    );
    let frame = extract_single_frame(&actions);
    let seg = parse_tcp_from_frame(&frame);
    assert_eq!(seg.flags(), TcpFlags::RST | TcpFlags::ACK);
}

#[test]
fn dns_denied_ip_returns_nxdomain_and_is_not_cached() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.host_policy
        .deny_ips
        .push(IpCidr::new(Ipv4Addr::new(93, 184, 216, 0), 24));
    let mut stack = NetworkStack::new(cfg);
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);

    let dns_query = build_dns_query(0x3333, "example.com", DnsType::A as u16);
    let dns_frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        53002,
        53,
        &dns_query,
    );
    let actions = stack.process_outbound_ethernet(&dns_frame, 0);
    let (dns_req_id, name) = match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        _ => panic!("expected single DnsResolve action, got {actions:?}"),
    };

    let resp_actions = stack.handle_dns_resolved(
        DnsResolved {
            request_id: dns_req_id,
            name,
            addr: Some(Ipv4Addr::new(93, 184, 216, 34)),
            ttl_secs: 60,
        },
        1,
    );
    let resp_frame = extract_single_frame(&resp_actions);
    assert_dns_response_has_rcode(&resp_frame, 0x3333, DnsResponseCode::NameError);

    // Should not cache denied IPs, so we expect another resolve request on the same query.
    let actions = stack.process_outbound_ethernet(&dns_frame, 2);
    assert!(matches!(
        actions.as_slice(),
        [Action::DnsResolve {
            request_id: _,
            name: _
        }]
    ));
}

#[test]
fn udp_proxy_send_and_receive_roundtrip() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let guest_port = 50000;
    let remote_port = 9999;

    let payload = b"hi";
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        remote_port,
        payload,
    );
    let actions = stack.process_outbound_ethernet(&frame, 0);
    assert_eq!(
        actions,
        vec![Action::UdpProxySend {
            transport: aero_net_stack::UdpTransport::WebRtc,
            src_port: guest_port,
            dst_ip: remote_ip,
            dst_port: remote_port,
            data: payload.to_vec(),
        }]
    );

    // Response from proxy -> guest.
    let resp_actions = stack.handle_udp_proxy_event(
        aero_net_stack::UdpProxyEvent {
            src_ip: remote_ip,
            src_port: remote_port,
            dst_port: guest_port,
            data: b"ok".to_vec(),
        },
        1,
    );
    let resp_frame = extract_single_frame(&resp_actions);
    let udp = parse_udp_from_frame(&resp_frame);
    assert_eq!(udp.src_port(), remote_port);
    assert_eq!(udp.dst_port(), guest_port);
    assert_eq!(udp.payload(), b"ok");
}

#[test]
fn udp_proxy_fallback_transport() {
    let cfg = StackConfig {
        webrtc_udp: false,
        ..StackConfig::default()
    };
    let mut stack = NetworkStack::new(cfg);
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);
    stack.set_network_enabled(true);

    let remote_ip = Ipv4Addr::new(203, 0, 113, 10);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        50001,
        9999,
        b"hi",
    );
    let actions = stack.process_outbound_ethernet(&frame, 0);
    assert_eq!(
        actions,
        vec![Action::UdpProxySend {
            transport: aero_net_stack::UdpTransport::Proxy,
            src_port: 50001,
            dst_ip: remote_ip,
            dst_port: 9999,
            data: b"hi".to_vec(),
        }]
    );
}

#[test]
fn tcp_connection_cap_rejects_new_syn() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.max_tcp_connections = 1;
    let mut stack = NetworkStack::new(cfg);
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);

    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let syn_a = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        40000,
        80,
        123,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions_a = stack.process_outbound_ethernet(&syn_a, 0);
    assert!(
        actions_a
            .iter()
            .any(|a| matches!(a, Action::TcpProxyConnect { .. })),
        "first SYN should be accepted"
    );

    let syn_b = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        40001,
        80,
        456,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions_b = stack.process_outbound_ethernet(&syn_b, 1);
    assert!(
        actions_b
            .iter()
            .all(|a| !matches!(a, Action::TcpProxyConnect { .. })),
        "second SYN should not allocate state"
    );
    let frame = extract_single_frame(&actions_b);
    let seg = parse_tcp_from_frame(&frame);
    assert!(seg.flags().contains(TcpFlags::RST | TcpFlags::ACK));
}

#[test]
fn tcp_buffered_payload_cap_sends_rst_and_frees_state() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.max_tcp_connections = 1;
    cfg.max_buffered_tcp_bytes_per_conn = 8;
    let mut stack = NetworkStack::new(cfg);
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);

    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let guest_port_a = 41000;
    let guest_isn = 1000;

    // SYN.
    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port_a,
        80,
        guest_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn, 0);
    let (conn_id, syn_ack_frame) = extract_tcp_connect_and_frame(&actions);
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame);

    // First payload chunk fits in the buffer.
    let payload = b"12345678";
    let psh_ok = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port_a,
        80,
        guest_isn + 1,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    let actions = stack.process_outbound_ethernet(&psh_ok, 1);
    assert!(actions.iter().any(|a| matches!(a, Action::EmitFrame(_))));
    assert!(actions
        .iter()
        .all(|a| !matches!(a, Action::TcpProxySend { .. })));

    // Second chunk would exceed the per-connection buffer limit -> RST + drop state.
    let overflow = b"x";
    let psh_overflow = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port_a,
        80,
        guest_isn + 1 + payload.len() as u32,
        syn_ack.seq_number() + 1,
        TcpFlags::ACK | TcpFlags::PSH,
        overflow,
    );
    let actions = stack.process_outbound_ethernet(&psh_overflow, 2);
    assert!(actions.iter().any(
        |a| matches!(a, Action::TcpProxyClose { connection_id } if *connection_id == conn_id)
    ));
    let frame = extract_single_frame(
        &actions
            .iter()
            .filter_map(|a| match a {
                Action::EmitFrame(f) => Some(Action::EmitFrame(f.clone())),
                _ => None,
            })
            .collect::<Vec<_>>(),
    );
    let seg = parse_tcp_from_frame(&frame);
    assert!(seg.flags().contains(TcpFlags::RST | TcpFlags::ACK));

    // Connection state should be gone, freeing the only available slot.
    let syn_b = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        41001,
        80,
        2000,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn_b, 3);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::TcpProxyConnect { .. })),
        "new SYN should be accepted after abort"
    );
}

#[test]
fn dns_cache_eviction_fifo() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.max_dns_cache_entries = 2;
    let mut stack = NetworkStack::new(cfg);
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
    dhcp_handshake(&mut stack, guest_mac);

    fn query_and_resolve(
        stack: &mut NetworkStack,
        guest_mac: MacAddr,
        txid: u16,
        name: &str,
        addr: Ipv4Addr,
        now_ms: u64,
    ) {
        let query = build_dns_query(txid, name, DnsType::A as u16);
        let frame = wrap_udp_ipv4_eth(
            guest_mac,
            stack.config().our_mac,
            stack.config().guest_ip,
            stack.config().dns_ip,
            53000 + txid,
            53,
            &query,
        );
        let actions = stack.process_outbound_ethernet(&frame, now_ms);
        let (req_id, got_name) = match actions.as_slice() {
            [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
            _ => panic!("expected single DnsResolve action, got {actions:?}"),
        };
        let resp_actions = stack.handle_dns_resolved(
            DnsResolved {
                request_id: req_id,
                name: got_name,
                addr: Some(addr),
                ttl_secs: 60,
            },
            now_ms + 1,
        );
        let resp_frame = extract_single_frame(&resp_actions);
        assert_dns_response_has_a_record(&resp_frame, txid, addr.octets());
    }

    query_and_resolve(
        &mut stack,
        guest_mac,
        0x1000,
        "a.example",
        Ipv4Addr::new(1, 1, 1, 1),
        0,
    );
    query_and_resolve(
        &mut stack,
        guest_mac,
        0x1001,
        "b.example",
        Ipv4Addr::new(2, 2, 2, 2),
        10,
    );
    query_and_resolve(
        &mut stack,
        guest_mac,
        0x1002,
        "c.example",
        Ipv4Addr::new(3, 3, 3, 3),
        20,
    );

    // a.example should have been evicted (FIFO), so another query should trigger a new resolve.
    let query_a = build_dns_query(0x2000, "a.example", DnsType::A as u16);
    let frame_a = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        54000,
        53,
        &query_a,
    );
    let actions = stack.process_outbound_ethernet(&frame_a, 30);
    assert!(matches!(actions.as_slice(), [Action::DnsResolve { .. }]));

    // b.example and c.example should still be cached.
    let query_b = build_dns_query(0x2001, "b.example", DnsType::A as u16);
    let frame_b = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        54001,
        53,
        &query_b,
    );
    let actions = stack.process_outbound_ethernet(&frame_b, 30);
    let resp_frame = extract_single_frame(&actions);
    assert_dns_response_has_a_record(&resp_frame, 0x2001, [2, 2, 2, 2]);

    let query_c = build_dns_query(0x2002, "c.example", DnsType::A as u16);
    let frame_c = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        54002,
        53,
        &query_c,
    );
    let actions = stack.process_outbound_ethernet(&frame_c, 30);
    let resp_frame = extract_single_frame(&actions);
    assert_dns_response_has_a_record(&resp_frame, 0x2002, [3, 3, 3, 3]);
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

    let actions = stack.process_outbound_ethernet(&discover_frame, 0);
    let offer_frames = extract_frames(&actions);
    assert_eq!(offer_frames.len(), 2);
    let offer_msg = parse_dhcp_from_frame(&offer_frames[0]);
    assert_eq!(offer_msg.message_type, DhcpMessageType::Offer);

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
    let actions = stack.process_outbound_ethernet(&request_frame, 1);
    let ack_frames = extract_frames(&actions);
    assert_eq!(ack_frames.len(), 2);
    let ack_msg = parse_dhcp_from_frame(&ack_frames[0]);
    assert_eq!(ack_msg.message_type, DhcpMessageType::Ack);
    assert!(stack.is_ip_assigned());
}

fn extract_single_frame(actions: &[Action]) -> Vec<u8> {
    let frames = extract_frames(actions);
    assert_eq!(frames.len(), 1, "expected 1 EmitFrame, got {actions:?}");
    frames.into_iter().next().unwrap()
}

fn extract_frames(actions: &[Action]) -> Vec<Vec<u8>> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::EmitFrame(f) => Some(f.clone()),
            _ => None,
        })
        .collect()
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

fn parse_dhcp_from_frame(frame: &[u8]) -> DhcpMessage {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    assert_eq!(ip.protocol(), Ipv4Protocol::UDP);
    let udp = UdpPacket::parse(ip.payload()).unwrap();
    assert_eq!(udp.src_port(), 67);
    assert_eq!(udp.dst_port(), 68);
    DhcpMessage::parse(udp.payload()).unwrap()
}

fn parse_tcp_from_frame(frame: &[u8]) -> TcpSegment<'_> {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    assert_eq!(ip.protocol(), Ipv4Protocol::TCP);
    TcpSegment::parse(ip.payload()).unwrap()
}

fn parse_udp_from_frame(frame: &[u8]) -> UdpPacket<'_> {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype(), EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    assert_eq!(ip.protocol(), Ipv4Protocol::UDP);
    UdpPacket::parse(ip.payload()).unwrap()
}

fn assert_dns_response_has_a_record(frame: &[u8], id: u16, addr: [u8; 4]) {
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    let udp = UdpPacket::parse(ip.payload()).unwrap();
    assert_eq!(udp.src_port(), 53);
    let dns = udp.payload();
    assert_eq!(&dns[0..2], &id.to_be_bytes());
    // QR=1
    assert_eq!(dns[2] & 0x80, 0x80);
    // ANCOUNT == 1
    assert_eq!(&dns[6..8], &1u16.to_be_bytes());
    // Answer RDATA is the final 4 bytes for our minimal response.
    assert_eq!(&dns[dns.len() - 4..], &addr);
}

fn assert_dns_response_has_rcode(frame: &[u8], id: u16, rcode: DnsResponseCode) {
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload()).unwrap();
    let udp = UdpPacket::parse(ip.payload()).unwrap();
    assert_eq!(udp.src_port(), 53);
    let dns = udp.payload();
    assert_eq!(&dns[0..2], &id.to_be_bytes());
    // QR=1
    assert_eq!(dns[2] & 0x80, 0x80);
    let flags = u16::from_be_bytes([dns[2], dns[3]]);
    assert_eq!(flags & 0x000f, rcode as u16);
    // No answers expected for error responses.
    assert_eq!(&dns[6..8], &0u16.to_be_bytes());
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
