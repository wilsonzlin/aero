use std::net::Ipv4Addr;

use aero_net_stack::packet::{
    EtherType, EthernetFrame, Ipv4Packet, Ipv4Protocol, MacAddr, UdpDatagram,
};
use emulator::io::net::stack::{Action, DnsResolved, IpCidr, NetStackBackend, StackConfig};

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

fn build_dns_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in name.split('.') {
        if label.is_empty() {
            continue;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // IN
    out
}

fn assert_dns_response_has_a_record(frame: &[u8], id: u16, addr: [u8; 4]) {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype, EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    assert_eq!(ip.protocol, Ipv4Protocol::UDP);
    let udp = UdpDatagram::parse(ip.payload).unwrap();
    assert_eq!(udp.src_port, 53);
    let dns = udp.payload;
    assert_eq!(&dns[0..2], &id.to_be_bytes());
    // QR=1
    assert_eq!(dns[2] & 0x80, 0x80);
    // ANCOUNT == 1
    assert_eq!(&dns[6..8], &1u16.to_be_bytes());
    // Answer RDATA is the final 4 bytes for our minimal response.
    assert_eq!(&dns[dns.len() - 4..], &addr);
}

fn assert_dns_response_has_rcode(frame: &[u8], id: u16, rcode: u16) {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype, EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    assert_eq!(ip.protocol, Ipv4Protocol::UDP);
    let udp = UdpDatagram::parse(ip.payload).unwrap();
    assert_eq!(udp.src_port, 53);
    let dns = udp.payload;
    assert_eq!(&dns[0..2], &id.to_be_bytes());
    // QR=1
    assert_eq!(dns[2] & 0x80, 0x80);
    let flags = u16::from_be_bytes([dns[2], dns[3]]);
    assert_eq!(flags & 0x000f, rcode);
    // No answers expected for error responses.
    assert_eq!(&dns[6..8], &0u16.to_be_bytes());
}

#[test]
fn dns_action_roundtrip_emits_response_frame() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let guest_mac = MacAddr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x02]);
    let mut backend = NetStackBackend::new(cfg.clone());

    let txid = 0x1234;
    let query = build_dns_query(txid, "Example.COM.", 1 /* A */);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        cfg.dns_ip,
        53000,
        53,
        &query,
    );
    backend.transmit_at(frame, 0);

    let actions = backend.drain_actions();
    let [Action::DnsResolve { request_id, name }] = actions.as_slice() else {
        panic!("expected single DnsResolve action, got {actions:?}");
    };
    assert_eq!(name, "example.com");

    let resolved_ip = Ipv4Addr::new(93, 184, 216, 34);
    backend.push_dns_resolved(
        DnsResolved {
            request_id: *request_id,
            name: name.clone(),
            addr: Some(resolved_ip),
            ttl_secs: 60,
        },
        1,
    );

    let frames = backend.drain_frames();
    assert_eq!(frames.len(), 1);
    assert_dns_response_has_a_record(&frames[0], txid, resolved_ip.octets());
}

#[test]
fn dns_denied_ip_returns_nxdomain_and_is_not_cached() {
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    cfg.host_policy
        .deny_ips
        .push(IpCidr::new(Ipv4Addr::new(93, 184, 216, 0), 24));
    let guest_mac = MacAddr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x03]);
    let mut backend = NetStackBackend::new(cfg.clone());

    let txid = 0x3333;
    let query = build_dns_query(txid, "example.com", 1 /* A */);
    let frame = wrap_udp_ipv4_eth(
        guest_mac,
        cfg.our_mac,
        cfg.guest_ip,
        cfg.dns_ip,
        53001,
        53,
        &query,
    );

    backend.transmit_at(frame.clone(), 0);
    let actions = backend.drain_actions();
    let [Action::DnsResolve { request_id, name }] = actions.as_slice() else {
        panic!("expected single DnsResolve action, got {actions:?}");
    };

    backend.push_dns_resolved(
        DnsResolved {
            request_id: *request_id,
            name: name.clone(),
            addr: Some(Ipv4Addr::new(93, 184, 216, 34)),
            ttl_secs: 60,
        },
        1,
    );
    let frames = backend.drain_frames();
    assert_eq!(frames.len(), 1);
    // NXDOMAIN (NameError) == 3
    assert_dns_response_has_rcode(&frames[0], txid, 3);

    // Denied IPs must not be cached, so the same query should trigger another resolve action.
    backend.transmit_at(frame, 2);
    let actions = backend.drain_actions();
    assert!(matches!(actions.as_slice(), [Action::DnsResolve { .. }]));
}
