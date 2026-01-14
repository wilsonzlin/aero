use aero_net_stack::packet::*;
use aero_net_stack::{Action, DnsResolved, NetworkStack, StackConfig};
use core::net::Ipv4Addr;

const GUEST_MAC: MacAddr = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);

#[test]
fn dns_cache_eviction_is_fifo() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.max_dns_cache_entries = 2;
    let mut stack = NetworkStack::new(cfg);

    query_and_resolve(
        &mut stack,
        0x1000,
        53_000,
        "a.example",
        Ipv4Addr::new(1, 1, 1, 1),
        0,
        60,
    );
    query_and_resolve(
        &mut stack,
        0x1001,
        53_001,
        "b.example",
        Ipv4Addr::new(2, 2, 2, 2),
        10,
        60,
    );
    // Inserting the third entry should evict the oldest (a.example), not b.example.
    query_and_resolve(
        &mut stack,
        0x1002,
        53_002,
        "c.example",
        Ipv4Addr::new(3, 3, 3, 3),
        20,
        60,
    );

    // a.example should have been evicted (FIFO), so another query should trigger a new resolve.
    let actions = send_dns_query(&mut stack, 0x2000, 54_000, "a.example", 30);
    assert!(
        matches!(actions.as_slice(), [Action::DnsResolve { .. }]),
        "expected eviction to drop a.example (FIFO), got {actions:?}"
    );

    // b.example and c.example should still be cached.
    let resp_b = extract_single_frame(&send_dns_query(&mut stack, 0x2001, 54_001, "b.example", 30));
    assert_dns_response_has_a_record(&resp_b, 0x2001, [2, 2, 2, 2]);
    let resp_c = extract_single_frame(&send_dns_query(&mut stack, 0x2002, 54_002, "c.example", 30));
    assert_dns_response_has_a_record(&resp_c, 0x2002, [3, 3, 3, 3]);
}

#[test]
fn dns_cache_hits_do_not_reorder_eviction_order() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.max_dns_cache_entries = 2;
    let mut stack = NetworkStack::new(cfg);

    query_and_resolve(
        &mut stack,
        0x1000,
        53_000,
        "a.example",
        Ipv4Addr::new(1, 1, 1, 1),
        0,
        60,
    );
    query_and_resolve(
        &mut stack,
        0x1001,
        53_001,
        "b.example",
        Ipv4Addr::new(2, 2, 2, 2),
        10,
        60,
    );

    // Cache hit for a.example (this must NOT affect FIFO eviction order).
    let resp_a = extract_single_frame(&send_dns_query(&mut stack, 0x2000, 54_000, "a.example", 20));
    assert_dns_response_has_a_record(&resp_a, 0x2000, [1, 1, 1, 1]);

    // Insert c.example and verify eviction still drops a.example, not b.example.
    query_and_resolve(
        &mut stack,
        0x1002,
        53_002,
        "c.example",
        Ipv4Addr::new(3, 3, 3, 3),
        30,
        60,
    );

    let actions = send_dns_query(&mut stack, 0x3000, 55_000, "a.example", 40);
    assert!(
        matches!(actions.as_slice(), [Action::DnsResolve { .. }]),
        "expected a.example to be evicted even after cache hit, got {actions:?}"
    );

    let resp_b = extract_single_frame(&send_dns_query(&mut stack, 0x3001, 55_001, "b.example", 40));
    assert_dns_response_has_a_record(&resp_b, 0x3001, [2, 2, 2, 2]);
}

#[test]
fn dns_cache_ttl_decrements_and_expires() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let mut stack = NetworkStack::new(cfg);

    let name = "ttl.example";
    let addr = Ipv4Addr::new(203, 0, 113, 7);

    // Seed the DNS cache with a short TTL.
    let (req_id, resolved_name) = query_expect_resolve(&mut stack, 0x1234, 53_000, name, 0);
    let resp = extract_single_frame(&stack.handle_dns_resolved(
        DnsResolved {
            request_id: req_id,
            name: resolved_name,
            addr: Some(addr),
            ttl_secs: 2,
        },
        0,
    ));
    assert_dns_response_has_a_record(&resp, 0x1234, addr.octets());
    assert_eq!(
        dns_answer_ttl_secs(&resp),
        2,
        "expected initial reply TTL to match resolved.ttl_secs"
    );

    // Query within the TTL window: should be served from cache, with a TTL reflecting remaining
    // lifetime (clamped by the u32 TTL field and floored to seconds).
    let resp_cached = extract_single_frame(&send_dns_query(&mut stack, 0x1235, 53_001, name, 500));
    assert_dns_response_has_a_record(&resp_cached, 0x1235, addr.octets());
    assert_eq!(dns_answer_ttl_secs(&resp_cached), 1);

    // Once we advance to the expiry timestamp, the entry should no longer be returned and a new
    // resolve should be requested.
    let actions = send_dns_query(&mut stack, 0x1236, 53_002, name, 2000);
    assert!(
        matches!(actions.as_slice(), [Action::DnsResolve { .. }]),
        "expected expired cache entry to trigger a new resolve, got {actions:?}"
    );
}

#[test]
fn pending_dns_cap_returns_servfail_and_does_not_allocate_state() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.max_pending_dns = 1;
    let mut stack = NetworkStack::new(cfg);

    // First query consumes the single pending DNS slot.
    let (req_id, name) = query_expect_resolve(&mut stack, 0x1111, 53_000, "one.example", 0);

    // Second query should be answered immediately with SERVFAIL (no new pending state allocated).
    let actions = send_dns_query(&mut stack, 0x1112, 53_001, "two.example", 1);
    assert!(
        actions
            .iter()
            .all(|a| !matches!(a, Action::DnsResolve { .. })),
        "expected no DnsResolve action when max_pending_dns exceeded, got {actions:?}"
    );
    let resp = extract_single_frame(&actions);
    assert_dns_response_has_rcode(&resp, 0x1112, DnsResponseCode::ServerFailure);

    // Resolving the first request should free the pending slot.
    let _ = extract_single_frame(&stack.handle_dns_resolved(
        DnsResolved {
            request_id: req_id,
            name,
            addr: Some(Ipv4Addr::new(192, 0, 2, 1)),
            ttl_secs: 60,
        },
        2,
    ));

    // A third query should now be accepted, and should receive the next request_id (the rejected
    // SERVFAIL query must not have allocated pending state or consumed an ID).
    let actions = send_dns_query(&mut stack, 0x1113, 53_002, "three.example", 3);
    match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => {
            assert_eq!(*request_id, 2);
            assert_eq!(name, "three.example");
        }
        _ => panic!("expected DnsResolve after freeing pending slot, got {actions:?}"),
    };
}

fn query_and_resolve(
    stack: &mut NetworkStack,
    txid: u16,
    src_port: u16,
    name: &str,
    addr: Ipv4Addr,
    now_ms: u64,
    ttl_secs: u32,
) {
    let (req_id, resolved_name) = query_expect_resolve(stack, txid, src_port, name, now_ms);
    let resp_frame = extract_single_frame(&stack.handle_dns_resolved(
        DnsResolved {
            request_id: req_id,
            name: resolved_name,
            addr: Some(addr),
            ttl_secs,
        },
        now_ms.saturating_add(1),
    ));
    assert_dns_response_has_a_record(&resp_frame, txid, addr.octets());
}

fn query_expect_resolve(
    stack: &mut NetworkStack,
    txid: u16,
    src_port: u16,
    name: &str,
    now_ms: u64,
) -> (u32, String) {
    let actions = send_dns_query(stack, txid, src_port, name, now_ms);
    match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        _ => panic!("expected single DnsResolve action, got {actions:?}"),
    }
}

fn send_dns_query(
    stack: &mut NetworkStack,
    txid: u16,
    src_port: u16,
    name: &str,
    now_ms: u64,
) -> Vec<Action> {
    let query = build_dns_query(txid, name, DnsType::A as u16);
    let frame = wrap_udp_ipv4_eth(
        GUEST_MAC,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        src_port,
        53,
        &query,
    );
    stack.process_outbound_ethernet(&frame, now_ms)
}

fn dns_answer_ttl_secs(frame: &[u8]) -> u32 {
    let udp = parse_udp_from_frame(frame);
    let dns_payload = udp.payload();
    let question = dns::parse_single_question(dns_payload).expect("parse_single_question");

    // DNS response is:
    // - 12-byte header
    // - question section: QNAME + QTYPE + QCLASS
    // - answer section (when present): NAMEPTR + TYPE + CLASS + TTL + RDLEN + RDATA
    let question_len = 12 + question.qname.len() + 4;
    let ttl_off = question_len + 2 + 2 + 2;
    u32::from_be_bytes(
        dns_payload[ttl_off..ttl_off + 4]
            .try_into()
            .expect("ttl bytes"),
    )
}

fn extract_single_frame(actions: &[Action]) -> Vec<u8> {
    let mut frames = actions.iter().filter_map(|a| match a {
        Action::EmitFrame(f) => Some(f.clone()),
        _ => None,
    });
    let frame = frames.next().expect("missing EmitFrame");
    assert!(
        frames.next().is_none(),
        "expected exactly 1 EmitFrame, got {actions:?}"
    );
    frame
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
