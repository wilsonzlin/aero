use core::net::Ipv4Addr;

use nt_packetlib::io::net::packet::{
    arp::{ArpPacket, ArpReplyFrameBuilder, ARP_OP_REPLY},
    dhcp::{DhcpMessage, DhcpMessageType, DhcpOfferAckBuilder, DHCP_MSG_OFFER},
    dns::{parse_single_query, DnsResponseBuilder, DnsResponseCode},
    ethernet::{EthernetFrame, EthernetFrameBuilder, ETHERTYPE_ARP, ETHERTYPE_IPV4},
    icmp::{IcmpEchoBuilder, Icmpv4Packet},
    ipv4::{Ipv4Packet, Ipv4PacketBuilder, IPPROTO_ICMP, IPPROTO_TCP, IPPROTO_UDP},
    tcp::{TcpFlags, TcpSegment, TcpSegmentBuilder},
    udp::{UdpPacket, UdpPacketBuilder},
    MacAddr,
};

#[test]
fn arp_reply_frame_roundtrip() {
    let sender_mac = MacAddr([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
    let target_mac = MacAddr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    let sender_ip = Ipv4Addr::new(10, 0, 0, 1);
    let target_ip = Ipv4Addr::new(10, 0, 0, 2);

    let frame = ArpReplyFrameBuilder {
        sender_mac,
        sender_ip,
        target_mac,
        target_ip,
    }
    .build_vec()
    .unwrap();

    let eth = EthernetFrame::parse(&frame).unwrap();
    assert_eq!(eth.ethertype(), ETHERTYPE_ARP);
    assert_eq!(eth.src_mac(), sender_mac);
    assert_eq!(eth.dest_mac(), target_mac);

    let arp = ArpPacket::parse(eth.payload()).unwrap();
    assert_eq!(arp.opcode(), ARP_OP_REPLY);
    assert_eq!(arp.sender_mac().unwrap(), sender_mac);
    assert_eq!(arp.target_mac().unwrap(), target_mac);
    assert_eq!(arp.sender_ip().unwrap(), sender_ip);
    assert_eq!(arp.target_ip().unwrap(), target_ip);
}

#[test]
fn ipv4_udp_dns_response_roundtrip() {
    // Build a minimal DNS query so we can re-use its QNAME bytes in the response.
    let query = [
        0x12, 0x34, 0x01, 0x00, // id + flags (RD)
        0x00, 0x01, 0x00, 0x00, // qdcount=1
        0x00, 0x00, 0x00, 0x00, // an/ns/ar = 0
        0x01, b'a', 0x00, // QNAME = "a."
        0x00, 0x01, 0x00, 0x01, // QTYPE=A, QCLASS=IN
    ];
    let q = parse_single_query(&query).unwrap();

    let dns = DnsResponseBuilder {
        id: q.id,
        rd: q.recursion_desired(),
        rcode: DnsResponseCode::NoError,
        qname: q.qname,
        qtype: q.qtype,
        qclass: q.qclass,
        answer_a: Some(Ipv4Addr::new(10, 0, 0, 1)),
        ttl: 60,
    }
    .build_vec()
    .unwrap();

    let src_ip = Ipv4Addr::new(10, 0, 0, 2);
    let dst_ip = Ipv4Addr::new(10, 0, 0, 15);
    let udp = UdpPacketBuilder {
        src_port: 53,
        dst_port: 12345,
        payload: &dns,
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();

    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0x4000,
        ttl: 64,
        protocol: IPPROTO_UDP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &udp,
    }
    .build_vec()
    .unwrap();

    let eth = EthernetFrameBuilder {
        dest_mac: MacAddr([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]),
        src_mac: MacAddr([0xde, 0xad, 0xbe, 0xef, 0x00, 0x02]),
        ethertype: ETHERTYPE_IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap();

    let eth_p = EthernetFrame::parse(&eth).unwrap();
    let ip_p = Ipv4Packet::parse(eth_p.payload()).unwrap();
    assert_eq!(ip_p.protocol(), IPPROTO_UDP);
    assert!(ip_p.checksum_valid());
    let udp_p = UdpPacket::parse(ip_p.payload()).unwrap();
    assert!(udp_p.checksum_valid_ipv4(src_ip, dst_ip));
    assert_eq!(udp_p.payload(), dns.as_slice());
}

#[test]
fn ipv4_udp_dhcp_offer_roundtrip() {
    let gateway_mac = MacAddr([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    let client_mac = MacAddr([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]);
    let server_ip = Ipv4Addr::new(10, 0, 2, 2);
    let offered_ip = Ipv4Addr::new(10, 0, 2, 15);

    let dhcp = DhcpOfferAckBuilder {
        message_type: DHCP_MSG_OFFER,
        transaction_id: 0x12345678,
        client_mac,
        your_ip: offered_ip,
        server_ip,
        subnet_mask: Ipv4Addr::new(255, 255, 255, 0),
        router: server_ip,
        dns_servers: &[server_ip],
        lease_time_secs: 86_400,
    }
    .build_vec()
    .unwrap();

    let dhcp_msg = DhcpMessage::parse(&dhcp).unwrap();
    assert_eq!(dhcp_msg.transaction_id, 0x12345678);
    assert_eq!(dhcp_msg.client_mac, client_mac);
    assert_eq!(dhcp_msg.message_type, DhcpMessageType::Offer);

    let src_ip = server_ip;
    let dst_ip = Ipv4Addr::BROADCAST;
    let udp = UdpPacketBuilder {
        src_port: 67,
        dst_port: 68,
        payload: &dhcp,
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();

    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 2,
        flags_fragment: 0x4000,
        ttl: 64,
        protocol: IPPROTO_UDP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &udp,
    }
    .build_vec()
    .unwrap();

    let eth = EthernetFrameBuilder {
        dest_mac: MacAddr::BROADCAST,
        src_mac: gateway_mac,
        ethertype: ETHERTYPE_IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap();

    let eth_p = EthernetFrame::parse(&eth).unwrap();
    let ip_p = Ipv4Packet::parse(eth_p.payload()).unwrap();
    assert!(ip_p.checksum_valid());
    let udp_p = UdpPacket::parse(ip_p.payload()).unwrap();
    assert!(udp_p.checksum_valid_ipv4(src_ip, dst_ip));
    assert_eq!(udp_p.payload(), dhcp.as_slice());
}

#[test]
fn ipv4_tcp_syn_ack_roundtrip() {
    let src_ip = Ipv4Addr::new(10, 0, 2, 2);
    let dst_ip = Ipv4Addr::new(10, 0, 2, 15);
    let tcp = TcpSegmentBuilder::syn_ack(80, 1234, 100, 200, 4096)
        .build_vec(src_ip, dst_ip)
        .unwrap();

    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 3,
        flags_fragment: 0x4000,
        ttl: 64,
        protocol: IPPROTO_TCP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &tcp,
    }
    .build_vec()
    .unwrap();

    let eth = EthernetFrameBuilder {
        dest_mac: MacAddr([0, 1, 2, 3, 4, 5]),
        src_mac: MacAddr([6, 7, 8, 9, 10, 11]),
        ethertype: ETHERTYPE_IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap();

    let eth_p = EthernetFrame::parse(&eth).unwrap();
    let ip_p = Ipv4Packet::parse(eth_p.payload()).unwrap();
    assert!(ip_p.checksum_valid());
    let seg = TcpSegment::parse(ip_p.payload()).unwrap();
    assert_eq!(seg.flags(), TcpFlags::SYN | TcpFlags::ACK);
    assert!(seg.checksum_valid_ipv4(src_ip, dst_ip));
}

#[test]
fn ipv4_icmp_echo_request_roundtrip() {
    let src_ip = Ipv4Addr::new(10, 0, 2, 15);
    let dst_ip = Ipv4Addr::new(10, 0, 2, 2);
    let icmp = IcmpEchoBuilder::echo_request(0x1234, 0x0001, b"ping")
        .build_vec()
        .unwrap();

    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 4,
        flags_fragment: 0x4000,
        ttl: 64,
        protocol: IPPROTO_ICMP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &icmp,
    }
    .build_vec()
    .unwrap();

    let eth = EthernetFrameBuilder {
        dest_mac: MacAddr([0, 1, 2, 3, 4, 5]),
        src_mac: MacAddr([6, 7, 8, 9, 10, 11]),
        ethertype: ETHERTYPE_IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap();

    let eth_p = EthernetFrame::parse(&eth).unwrap();
    let ip_p = Ipv4Packet::parse(eth_p.payload()).unwrap();
    assert!(ip_p.checksum_valid());
    let pkt = Icmpv4Packet::parse(ip_p.payload()).unwrap();
    assert!(pkt.checksum_valid());
    let echo = pkt.echo().unwrap();
    assert_eq!(echo.icmp_type, 8);
    assert_eq!(echo.identifier, 0x1234);
    assert_eq!(echo.sequence, 0x0001);
    assert_eq!(echo.payload, b"ping");
}
