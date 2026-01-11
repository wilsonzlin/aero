use aero_net_stack::packet::*;
use core::net::Ipv4Addr;

#[test]
fn ethernet_roundtrip() {
    let payload = [1u8, 2, 3, 4];
    let frame = EthernetFrameBuilder {
        dest_mac: MacAddr([0, 1, 2, 3, 4, 5]),
        src_mac: MacAddr([6, 7, 8, 9, 10, 11]),
        ethertype: EtherType::IPV4,
        payload: &payload,
    }
    .build_vec()
    .unwrap();
    let parsed = EthernetFrame::parse(&frame).unwrap();
    assert_eq!(parsed.dest_mac(), MacAddr([0, 1, 2, 3, 4, 5]));
    assert_eq!(parsed.src_mac(), MacAddr([6, 7, 8, 9, 10, 11]));
    assert_eq!(parsed.ethertype(), EtherType::IPV4);
    assert_eq!(parsed.payload(), &payload);
}

#[test]
fn arp_roundtrip() {
    let pkt = ArpPacketBuilder {
        opcode: ARP_OP_REQUEST,
        sender_mac: MacAddr([1, 2, 3, 4, 5, 6]),
        sender_ip: Ipv4Addr::new(10, 0, 0, 1),
        target_mac: MacAddr([0; 6]),
        target_ip: Ipv4Addr::new(10, 0, 0, 2),
    }
    .build_vec()
    .unwrap();

    let parsed = ArpPacket::parse(&pkt).unwrap();
    assert_eq!(parsed.htype(), HTYPE_ETHERNET);
    assert_eq!(parsed.ptype(), PTYPE_IPV4);
    assert_eq!(parsed.opcode(), ARP_OP_REQUEST);
    assert_eq!(parsed.sender_mac().unwrap(), MacAddr([1, 2, 3, 4, 5, 6]));
    assert_eq!(parsed.sender_ip().unwrap(), Ipv4Addr::new(10, 0, 0, 1));
    assert_eq!(parsed.target_ip().unwrap(), Ipv4Addr::new(10, 0, 0, 2));
}

#[test]
fn ipv4_udp_roundtrip() {
    let src_ip = Ipv4Addr::new(10, 0, 0, 1);
    let dst_ip = Ipv4Addr::new(10, 0, 0, 2);

    let udp = UdpPacketBuilder {
        src_port: 1234,
        dst_port: 5678,
        payload: b"hello",
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();

    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 42,
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

    let parsed_ip = Ipv4Packet::parse(&ip).unwrap();
    assert_eq!(parsed_ip.src_ip(), src_ip);
    assert_eq!(parsed_ip.dst_ip(), dst_ip);
    assert_eq!(parsed_ip.flags_fragment(), 0x4000);
    assert_eq!(parsed_ip.protocol(), Ipv4Protocol::UDP);
    assert!(parsed_ip.checksum_valid());

    let parsed_udp = UdpPacket::parse(parsed_ip.payload()).unwrap();
    assert_eq!(parsed_udp.src_port(), 1234);
    assert_eq!(parsed_udp.dst_port(), 5678);
    assert_eq!(parsed_udp.payload(), b"hello");
    assert!(parsed_udp.checksum_valid_ipv4(src_ip, dst_ip));
}

#[test]
fn ipv4_tcp_roundtrip() {
    let src_ip = Ipv4Addr::new(192, 0, 2, 1);
    let dst_ip = Ipv4Addr::new(198, 51, 100, 2);

    let tcp = TcpSegmentBuilder {
        src_port: 1111,
        dst_port: 2222,
        seq_number: 1,
        ack_number: 2,
        flags: TcpFlags::PSH | TcpFlags::ACK,
        window_size: 4096,
        urgent_pointer: 0,
        options: &[],
        payload: b"payload",
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();

    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 7,
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

    let parsed_ip = Ipv4Packet::parse(&ip).unwrap();
    assert!(parsed_ip.checksum_valid());
    let parsed_tcp = TcpSegment::parse(parsed_ip.payload()).unwrap();
    assert_eq!(parsed_tcp.src_port(), 1111);
    assert_eq!(parsed_tcp.dst_port(), 2222);
    assert_eq!(parsed_tcp.seq_number(), 1);
    assert_eq!(parsed_tcp.ack_number(), 2);
    assert_eq!(parsed_tcp.flags(), TcpFlags::PSH | TcpFlags::ACK);
    assert_eq!(parsed_tcp.payload(), b"payload");
    assert!(parsed_tcp.checksum_valid_ipv4(src_ip, dst_ip));
}

#[test]
fn dns_query_parse_and_response_build() {
    let query = build_dns_query(0x1234, "example.com");
    let parsed = parse_single_query(&query).unwrap();
    assert_eq!(parsed.id, 0x1234);
    assert_eq!(qname_to_string(parsed.qname).unwrap(), "example.com");

    let response = DnsResponseBuilder {
        id: 0x1234,
        rd: true,
        rcode: DnsResponseCode::NoError,
        qname: parsed.qname,
        qtype: parsed.qtype,
        qclass: parsed.qclass,
        answer_a: Some(Ipv4Addr::new(93, 184, 216, 34)),
        ttl: 60,
    }
    .build_vec()
    .unwrap();
    assert_eq!(&response[0..2], &0x1234u16.to_be_bytes());
    // QR=1
    assert_eq!(response[2] & 0x80, 0x80);
}

#[test]
fn icmp_echo_reply_roundtrip() {
    let payload = *b"ping";
    let icmp = IcmpEchoBuilder::echo_reply(0x1234, 0x0001, &payload)
        .build_vec()
        .unwrap();
    let pkt = Icmpv4Packet::parse(&icmp).unwrap();
    assert!(pkt.checksum_valid());
    let echo = pkt.echo().unwrap();
    assert_eq!(echo.icmp_type, 0);
    assert_eq!(echo.identifier, 0x1234);
    assert_eq!(echo.sequence, 0x0001);
    assert_eq!(echo.payload, &payload);
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
