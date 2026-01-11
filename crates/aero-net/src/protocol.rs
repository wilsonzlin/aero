use std::net::Ipv4Addr;

use nt_packetlib::packet::{
    arp as nt_arp, dhcp as nt_dhcp, dns as nt_dns, ethernet as nt_ethernet, ipv4 as nt_ipv4,
    tcp as nt_tcp, udp as nt_udp, MacAddr as NtMacAddr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xff; 6]);

    pub const fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 6] {
        &self.0
    }
}

fn to_nt_mac(mac: MacAddr) -> NtMacAddr {
    NtMacAddr(mac.0)
}

fn from_nt_mac(mac: NtMacAddr) -> MacAddr {
    MacAddr(mac.0)
}

pub const ETHERTYPE_IPV4: u16 = nt_ethernet::ETHERTYPE_IPV4;
pub const ETHERTYPE_ARP: u16 = nt_ethernet::ETHERTYPE_ARP;

#[derive(Debug)]
pub struct EthernetFrame<'a> {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

pub fn parse_ethernet_frame(frame: &[u8]) -> Option<EthernetFrame<'_>> {
    let parsed = nt_ethernet::EthernetFrame::parse(frame).ok()?;
    Some(EthernetFrame {
        dst: from_nt_mac(parsed.dest_mac()),
        src: from_nt_mac(parsed.src_mac()),
        ethertype: parsed.ethertype(),
        payload: parsed.payload(),
    })
}

pub fn build_ethernet_frame(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    nt_ethernet::EthernetFrameBuilder {
        dest_mac: to_nt_mac(dst),
        src_mac: to_nt_mac(src),
        ethertype,
        payload,
    }
    .build_vec()
    .expect("ethernet frame builder should not fail")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArpOp {
    Request = 1,
    Reply = 2,
}

#[derive(Debug)]
pub struct ArpPacket {
    pub op: ArpOp,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

pub fn parse_arp_packet(payload: &[u8]) -> Option<ArpPacket> {
    let arp = nt_arp::ArpPacket::parse(payload).ok()?;
    if arp.htype() != nt_arp::HTYPE_ETHERNET
        || arp.ptype() != nt_arp::PTYPE_IPV4
        || arp.hlen() != 6
        || arp.plen() != 4
    {
        return None;
    }
    let op = match arp.opcode() {
        nt_arp::ARP_OP_REQUEST => ArpOp::Request,
        nt_arp::ARP_OP_REPLY => ArpOp::Reply,
        _ => return None,
    };
    Some(ArpPacket {
        op,
        sender_mac: from_nt_mac(arp.sender_mac()?),
        sender_ip: arp.sender_ip()?,
        target_mac: from_nt_mac(arp.target_mac()?),
        target_ip: arp.target_ip()?,
    })
}

pub fn build_arp_packet(
    op: ArpOp,
    sender_mac: MacAddr,
    sender_ip: Ipv4Addr,
    target_mac: MacAddr,
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    nt_arp::ArpPacketBuilder {
        opcode: op as u16,
        sender_mac: to_nt_mac(sender_mac),
        sender_ip,
        target_mac: to_nt_mac(target_mac),
        target_ip,
    }
    .build_vec()
    .expect("arp builder should not fail")
}

#[derive(Debug)]
pub struct Ipv4Packet<'a> {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub protocol: u8,
    pub payload: &'a [u8],
}

pub fn parse_ipv4_packet(packet: &[u8]) -> Option<Ipv4Packet<'_>> {
    let ip = nt_ipv4::Ipv4Packet::parse(packet).ok()?;
    if !ip.checksum_valid() {
        return None;
    }
    Some(Ipv4Packet {
        src: ip.src_ip(),
        dst: ip.dst_ip(),
        protocol: ip.protocol(),
        payload: ip.payload(),
    })
}

pub fn build_ipv4_packet(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    nt_ipv4::Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 0,
        flags_fragment: 0,
        ttl: 64,
        protocol,
        src_ip: src,
        dst_ip: dst,
        options: &[],
        payload,
    }
    .build_vec()
    .expect("ipv4 builder should not fail")
}

#[derive(Debug)]
pub struct UdpPacket<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: &'a [u8],
}

pub fn parse_udp_packet(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, packet: &[u8]) -> Option<UdpPacket<'_>> {
    let udp = nt_udp::UdpPacket::parse(packet).ok()?;
    if !udp.checksum_valid_ipv4(src_ip, dst_ip) {
        return None;
    }
    Some(UdpPacket {
        src_port: udp.src_port(),
        dst_port: udp.dst_port(),
        payload: udp.payload(),
    })
}

pub fn build_udp_packet(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    nt_udp::UdpPacketBuilder {
        src_port,
        dst_port,
        payload,
    }
    .build_vec(src_ip, dst_ip)
    .expect("udp builder should not fail")
}

pub const IP_PROTO_TCP: u8 = nt_ipv4::IPPROTO_TCP;
pub const IP_PROTO_UDP: u8 = nt_ipv4::IPPROTO_UDP;

pub const TCP_FLAG_FIN: u16 = nt_tcp::TcpFlags::FIN.0;
pub const TCP_FLAG_SYN: u16 = nt_tcp::TcpFlags::SYN.0;
pub const TCP_FLAG_RST: u16 = nt_tcp::TcpFlags::RST.0;
pub const TCP_FLAG_PSH: u16 = nt_tcp::TcpFlags::PSH.0;
pub const TCP_FLAG_ACK: u16 = nt_tcp::TcpFlags::ACK.0;

#[derive(Debug)]
pub struct TcpSegment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u16,
    pub window: u16,
    pub payload: &'a [u8],
}

pub fn parse_tcp_segment(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    segment: &[u8],
) -> Option<TcpSegment<'_>> {
    let seg = nt_tcp::TcpSegment::parse(segment).ok()?;
    if !seg.checksum_valid_ipv4(src_ip, dst_ip) {
        return None;
    }
    Some(TcpSegment {
        src_port: seg.src_port(),
        dst_port: seg.dst_port(),
        seq: seg.seq_number(),
        ack: seg.ack_number(),
        flags: seg.flags().0,
        window: seg.window_size(),
        payload: seg.payload(),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn build_tcp_segment(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    window: u16,
    payload: &[u8],
) -> Vec<u8> {
    nt_tcp::TcpSegmentBuilder {
        src_port,
        dst_port,
        seq_number: seq,
        ack_number: ack,
        flags: nt_tcp::TcpFlags(flags),
        window_size: window,
        urgent_pointer: 0,
        options: &[],
        payload,
    }
    .build_vec(src_ip, dst_ip)
    .expect("tcp builder should not fail")
}

pub const DHCP_CLIENT_PORT: u16 = 68;
pub const DHCP_SERVER_PORT: u16 = 67;

pub const DHCP_MAGIC_COOKIE: [u8; 4] = nt_dhcp::DHCP_MAGIC_COOKIE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpMessageType {
    Discover = 1,
    Offer = 2,
    Request = 3,
    Ack = 5,
}

#[derive(Debug)]
pub struct DhcpParsed {
    pub xid: u32,
    pub chaddr: MacAddr,
    pub yiaddr: Ipv4Addr,
    pub message_type: DhcpMessageType,
    pub requested_ip: Option<Ipv4Addr>,
    pub server_id: Option<Ipv4Addr>,
}

pub fn parse_dhcp(payload: &[u8]) -> Option<DhcpParsed> {
    let msg = nt_dhcp::DhcpMessage::parse(payload).ok()?;
    let yiaddr = Ipv4Addr::new(payload[16], payload[17], payload[18], payload[19]);
    let message_type = match msg.message_type {
        nt_dhcp::DhcpMessageType::Discover => DhcpMessageType::Discover,
        nt_dhcp::DhcpMessageType::Offer => DhcpMessageType::Offer,
        nt_dhcp::DhcpMessageType::Request => DhcpMessageType::Request,
        nt_dhcp::DhcpMessageType::Ack => DhcpMessageType::Ack,
        _ => return None,
    };
    Some(DhcpParsed {
        xid: msg.transaction_id,
        chaddr: from_nt_mac(msg.client_mac),
        yiaddr,
        message_type,
        requested_ip: msg.requested_ip,
        server_id: msg.server_identifier,
    })
}

pub fn build_dhcp_discover(xid: u32, chaddr: MacAddr) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // ethernet
    out[2] = 6;
    out[3] = 0;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10] = 0x80; // broadcast flag
    out[28..34].copy_from_slice(chaddr.as_bytes());
    out[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
    // options
    out.extend_from_slice(&[53, 1, DhcpMessageType::Discover as u8]);
    out.extend_from_slice(&[55, 3, 1, 3, 6]); // parameter request list
    out.push(255);
    out
}

pub fn build_dhcp_request(
    xid: u32,
    chaddr: MacAddr,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // ethernet
    out[2] = 6;
    out[3] = 0;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10] = 0x80; // broadcast
    out[28..34].copy_from_slice(chaddr.as_bytes());
    out[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
    // options
    out.extend_from_slice(&[53, 1, DhcpMessageType::Request as u8]);
    out.extend_from_slice(&[50, 4]);
    out.extend_from_slice(&requested_ip.octets());
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&server_id.octets());
    out.push(255);
    out
}

#[allow(clippy::too_many_arguments)]
pub fn build_dhcp_offer(
    xid: u32,
    chaddr: MacAddr,
    yiaddr: Ipv4Addr,
    server_ip: Ipv4Addr,
    netmask: Ipv4Addr,
    router: Ipv4Addr,
    dns: Ipv4Addr,
    lease_time_secs: u32,
) -> Vec<u8> {
    let dns_servers = [dns];
    nt_dhcp::DhcpOfferAckBuilder {
        message_type: nt_dhcp::DHCP_MSG_OFFER,
        transaction_id: xid,
        flags: 0,
        client_mac: to_nt_mac(chaddr),
        your_ip: yiaddr,
        server_ip,
        subnet_mask: netmask,
        router,
        dns_servers: &dns_servers,
        lease_time_secs,
    }
    .build_vec()
    .expect("dhcp offer builder should not fail")
}

#[allow(clippy::too_many_arguments)]
pub fn build_dhcp_ack(
    xid: u32,
    chaddr: MacAddr,
    yiaddr: Ipv4Addr,
    server_ip: Ipv4Addr,
    netmask: Ipv4Addr,
    router: Ipv4Addr,
    dns: Ipv4Addr,
    lease_time_secs: u32,
) -> Vec<u8> {
    let dns_servers = [dns];
    nt_dhcp::DhcpOfferAckBuilder {
        message_type: nt_dhcp::DHCP_MSG_ACK,
        transaction_id: xid,
        flags: 0,
        client_mac: to_nt_mac(chaddr),
        your_ip: yiaddr,
        server_ip,
        subnet_mask: netmask,
        router,
        dns_servers: &dns_servers,
        lease_time_secs,
    }
    .build_vec()
    .expect("dhcp ack builder should not fail")
}

#[derive(Debug)]
pub struct DnsQuery {
    pub id: u16,
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

pub fn parse_dns_query(packet: &[u8]) -> Option<DnsQuery> {
    let q = nt_dns::parse_single_question(packet).ok()?;
    let name = nt_dns::qname_to_string(q.qname).ok()?;
    Some(DnsQuery {
        id: q.id,
        name,
        qtype: q.qtype,
        qclass: q.qclass,
    })
}

fn dns_qname_bytes(name: &str) -> Vec<u8> {
    // This intentionally mirrors our previous behavior: the input is assumed to be a simple
    // dot-separated hostname and we emit an uncompressed QNAME (labels + 0 terminator).
    let mut out = Vec::new();
    for label in name.split('.') {
        if label.is_empty() {
            continue;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

pub fn build_dns_response_a(id: u16, query: &DnsQuery, ip: Ipv4Addr) -> Vec<u8> {
    let qname = dns_qname_bytes(&query.name);
    nt_dns::DnsResponseBuilder {
        id,
        rd: true,
        rcode: nt_dns::DnsResponseCode::NoError,
        qname: &qname,
        qtype: query.qtype,
        qclass: query.qclass,
        answer_a: Some(ip),
        ttl: 60,
    }
    .build_vec()
    .expect("dns response builder should not fail")
}

pub fn build_dns_response_nxdomain(id: u16, query: &DnsQuery) -> Vec<u8> {
    let qname = dns_qname_bytes(&query.name);
    nt_dns::DnsResponseBuilder {
        id,
        rd: true,
        rcode: nt_dns::DnsResponseCode::NameError,
        qname: &qname,
        qtype: query.qtype,
        qclass: query.qclass,
        answer_a: None,
        ttl: 0,
    }
    .build_vec()
    .expect("dns response builder should not fail")
}

