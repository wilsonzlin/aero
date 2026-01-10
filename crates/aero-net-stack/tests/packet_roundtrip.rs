use aero_net_stack::packet::*;
use core::net::Ipv4Addr;

#[test]
fn ethernet_roundtrip() {
    let payload = [1u8, 2, 3, 4];
    let frame = EthernetFrame::serialize(
        MacAddr([0, 1, 2, 3, 4, 5]),
        MacAddr([6, 7, 8, 9, 10, 11]),
        EtherType::IPV4,
        &payload,
    );
    let parsed = EthernetFrame::parse(&frame).unwrap();
    assert_eq!(parsed.dst, MacAddr([0, 1, 2, 3, 4, 5]));
    assert_eq!(parsed.src, MacAddr([6, 7, 8, 9, 10, 11]));
    assert_eq!(parsed.ethertype, EtherType::IPV4);
    assert_eq!(parsed.payload, payload);
}

#[test]
fn arp_roundtrip() {
    let pkt = ArpPacket {
        op: ArpOperation::Request,
        sender_hw: MacAddr([1, 2, 3, 4, 5, 6]),
        sender_ip: Ipv4Addr::new(10, 0, 0, 1),
        target_hw: MacAddr([0; 6]),
        target_ip: Ipv4Addr::new(10, 0, 0, 2),
    };
    let bytes = pkt.serialize();
    let parsed = ArpPacket::parse(&bytes).unwrap();
    assert_eq!(parsed, pkt);
}

#[test]
fn ipv4_udp_roundtrip() {
    let udp = UdpDatagram::serialize(
        Ipv4Addr::new(10, 0, 0, 1),
        Ipv4Addr::new(10, 0, 0, 2),
        1234,
        5678,
        b"hello",
    );
    let ip = Ipv4Packet::serialize(
        Ipv4Addr::new(10, 0, 0, 1),
        Ipv4Addr::new(10, 0, 0, 2),
        Ipv4Protocol::UDP,
        42,
        64,
        &udp,
    );
    let parsed_ip = Ipv4Packet::parse(&ip).unwrap();
    assert_eq!(parsed_ip.src, Ipv4Addr::new(10, 0, 0, 1));
    assert_eq!(parsed_ip.dst, Ipv4Addr::new(10, 0, 0, 2));
    assert_eq!(parsed_ip.protocol, Ipv4Protocol::UDP);
    let parsed_udp = UdpDatagram::parse(parsed_ip.payload).unwrap();
    assert_eq!(parsed_udp.src_port, 1234);
    assert_eq!(parsed_udp.dst_port, 5678);
    assert_eq!(parsed_udp.payload, b"hello");
}

#[test]
fn ipv4_tcp_roundtrip() {
    let tcp = TcpSegment::serialize(
        Ipv4Addr::new(192, 0, 2, 1),
        Ipv4Addr::new(198, 51, 100, 2),
        1111,
        2222,
        1,
        2,
        TcpFlags::PSH | TcpFlags::ACK,
        4096,
        b"payload",
    );
    let ip = Ipv4Packet::serialize(
        Ipv4Addr::new(192, 0, 2, 1),
        Ipv4Addr::new(198, 51, 100, 2),
        Ipv4Protocol::TCP,
        7,
        64,
        &tcp,
    );
    let parsed_ip = Ipv4Packet::parse(&ip).unwrap();
    let parsed_tcp = TcpSegment::parse(parsed_ip.payload).unwrap();
    assert_eq!(parsed_tcp.src_port, 1111);
    assert_eq!(parsed_tcp.dst_port, 2222);
    assert_eq!(parsed_tcp.seq, 1);
    assert_eq!(parsed_tcp.ack, 2);
    assert_eq!(parsed_tcp.flags, TcpFlags::PSH | TcpFlags::ACK);
    assert_eq!(parsed_tcp.payload, b"payload");
}

#[test]
fn dns_query_parse_and_response_build() {
    let query = build_dns_query(0x1234, "example.com");
    let parsed = DnsMessage::parse_query(&query).unwrap();
    assert_eq!(parsed.id, 0x1234);
    assert_eq!(parsed.questions.len(), 1);
    assert_eq!(parsed.questions[0].name, "example.com");

    let response = DnsMessage::build_a_response(
        0x1234,
        true,
        "example.com",
        Some([93, 184, 216, 34]),
        60,
        DnsResponseCode::NoError,
    );
    assert_eq!(&response[0..2], &0x1234u16.to_be_bytes());
    // QR=1
    assert_eq!(response[2] & 0x80, 0x80);
}

fn build_dns_query(id: u16, name: &str) -> Vec<u8> {
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
    out.extend_from_slice(&(DnsType::A as u16).to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}
