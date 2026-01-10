use std::net::Ipv4Addr;

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

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;

#[derive(Debug)]
pub struct EthernetFrame<'a> {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

pub fn parse_ethernet_frame(frame: &[u8]) -> Option<EthernetFrame<'_>> {
    if frame.len() < 14 {
        return None;
    }
    let dst = MacAddr([frame[0], frame[1], frame[2], frame[3], frame[4], frame[5]]);
    let src = MacAddr([frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]]);
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    Some(EthernetFrame {
        dst,
        src,
        ethertype,
        payload: &frame[14..],
    })
}

pub fn build_ethernet_frame(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + payload.len());
    out.extend_from_slice(dst.as_bytes());
    out.extend_from_slice(src.as_bytes());
    out.extend_from_slice(&ethertype.to_be_bytes());
    out.extend_from_slice(payload);
    out
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
    if payload.len() < 28 {
        return None;
    }
    let htype = u16::from_be_bytes([payload[0], payload[1]]);
    let ptype = u16::from_be_bytes([payload[2], payload[3]]);
    let hlen = payload[4];
    let plen = payload[5];
    if htype != 1 || ptype != ETHERTYPE_IPV4 || hlen != 6 || plen != 4 {
        return None;
    }
    let op = match u16::from_be_bytes([payload[6], payload[7]]) {
        1 => ArpOp::Request,
        2 => ArpOp::Reply,
        _ => return None,
    };
    let sender_mac = MacAddr([
        payload[8],
        payload[9],
        payload[10],
        payload[11],
        payload[12],
        payload[13],
    ]);
    let sender_ip = Ipv4Addr::new(payload[14], payload[15], payload[16], payload[17]);
    let target_mac = MacAddr([
        payload[18],
        payload[19],
        payload[20],
        payload[21],
        payload[22],
        payload[23],
    ]);
    let target_ip = Ipv4Addr::new(payload[24], payload[25], payload[26], payload[27]);
    Some(ArpPacket {
        op,
        sender_mac,
        sender_ip,
        target_mac,
        target_ip,
    })
}

pub fn build_arp_packet(
    op: ArpOp,
    sender_mac: MacAddr,
    sender_ip: Ipv4Addr,
    target_mac: MacAddr,
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(28);
    out.extend_from_slice(&1u16.to_be_bytes()); // ethernet
    out.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    out.push(6);
    out.push(4);
    out.extend_from_slice(&(op as u16).to_be_bytes());
    out.extend_from_slice(sender_mac.as_bytes());
    out.extend_from_slice(&sender_ip.octets());
    out.extend_from_slice(target_mac.as_bytes());
    out.extend_from_slice(&target_ip.octets());
    out
}

#[derive(Debug)]
pub struct Ipv4Packet<'a> {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub protocol: u8,
    pub payload: &'a [u8],
}

pub fn parse_ipv4_packet(packet: &[u8]) -> Option<Ipv4Packet<'_>> {
    if packet.len() < 20 {
        return None;
    }
    let version = packet[0] >> 4;
    let ihl = (packet[0] & 0x0f) as usize;
    if version != 4 || ihl < 5 {
        return None;
    }
    let header_len = ihl * 4;
    if packet.len() < header_len {
        return None;
    }
    let total_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if total_len < header_len || packet.len() < total_len {
        return None;
    }
    // Verify header checksum
    if internet_checksum(&packet[..header_len]) != 0 {
        return None;
    }
    let protocol = packet[9];
    let src = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
    let dst = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
    Some(Ipv4Packet {
        src,
        dst,
        protocol,
        payload: &packet[header_len..total_len],
    })
}

pub fn build_ipv4_packet(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    let header_len = 20usize;
    let total_len = header_len + payload.len();
    let mut out = vec![0u8; header_len];
    out[0] = (4 << 4) | 5; // version + IHL
    out[1] = 0;
    out[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    out[4..6].copy_from_slice(&0u16.to_be_bytes()); // identification
    out[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags/fragment
    out[8] = 64; // ttl
    out[9] = protocol;
    out[10..12].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out[12..16].copy_from_slice(&src.octets());
    out[16..20].copy_from_slice(&dst.octets());
    let checksum = internet_checksum(&out);
    out[10..12].copy_from_slice(&checksum.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[derive(Debug)]
pub struct UdpPacket<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: &'a [u8],
}

pub fn parse_udp_packet(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    packet: &[u8],
) -> Option<UdpPacket<'_>> {
    if packet.len() < 8 {
        return None;
    }
    let src_port = u16::from_be_bytes([packet[0], packet[1]]);
    let dst_port = u16::from_be_bytes([packet[2], packet[3]]);
    let len = u16::from_be_bytes([packet[4], packet[5]]) as usize;
    if len < 8 || packet.len() < len {
        return None;
    }
    let checksum = u16::from_be_bytes([packet[6], packet[7]]);
    if checksum != 0 {
        let mut buf = packet[..len].to_vec();
        buf[6] = 0;
        buf[7] = 0;
        let computed = transport_checksum_ipv4(src_ip, dst_ip, 17, &buf);
        let computed = if computed == 0 { 0xffff } else { computed };
        if computed != checksum {
            return None;
        }
    }
    Some(UdpPacket {
        src_port,
        dst_port,
        payload: &packet[8..len],
    })
}

pub fn build_udp_packet(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let len = 8 + payload.len();
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&src_port.to_be_bytes());
    out.extend_from_slice(&dst_port.to_be_bytes());
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out.extend_from_slice(payload);

    let checksum = transport_checksum_ipv4(src_ip, dst_ip, 17, &out);
    let checksum = if checksum == 0 { 0xffff } else { checksum };
    out[6..8].copy_from_slice(&checksum.to_be_bytes());
    out
}

pub const IP_PROTO_TCP: u8 = 6;
pub const IP_PROTO_UDP: u8 = 17;

pub const TCP_FLAG_FIN: u16 = 0x01;
pub const TCP_FLAG_SYN: u16 = 0x02;
pub const TCP_FLAG_RST: u16 = 0x04;
pub const TCP_FLAG_PSH: u16 = 0x08;
pub const TCP_FLAG_ACK: u16 = 0x10;

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
    if segment.len() < 20 {
        return None;
    }
    let src_port = u16::from_be_bytes([segment[0], segment[1]]);
    let dst_port = u16::from_be_bytes([segment[2], segment[3]]);
    let seq = u32::from_be_bytes([segment[4], segment[5], segment[6], segment[7]]);
    let ack = u32::from_be_bytes([segment[8], segment[9], segment[10], segment[11]]);
    let data_offset = (segment[12] >> 4) as usize;
    if data_offset < 5 {
        return None;
    }
    let header_len = data_offset * 4;
    if segment.len() < header_len {
        return None;
    }
    let flags = (segment[13] as u16) | (((segment[12] & 0x01) as u16) << 8);
    let window = u16::from_be_bytes([segment[14], segment[15]]);
    let checksum = u16::from_be_bytes([segment[16], segment[17]]);

    let mut buf = segment.to_vec();
    buf[16] = 0;
    buf[17] = 0;
    let computed = transport_checksum_ipv4(src_ip, dst_ip, 6, &buf);
    if computed != checksum {
        return None;
    }

    Some(TcpSegment {
        src_port,
        dst_port,
        seq,
        ack,
        flags,
        window,
        payload: &segment[header_len..],
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
    let header_len = 20usize;
    let mut out = Vec::with_capacity(header_len + payload.len());
    out.extend_from_slice(&src_port.to_be_bytes());
    out.extend_from_slice(&dst_port.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ack.to_be_bytes());
    out.push((5u8) << 4); // data offset, no options
    out.push((flags & 0xff) as u8);
    out.extend_from_slice(&window.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer
    out.extend_from_slice(payload);

    let checksum = transport_checksum_ipv4(src_ip, dst_ip, 6, &out);
    out[16..18].copy_from_slice(&checksum.to_be_bytes());
    out
}

pub fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&b) = chunks.remainder().first() {
        sum += (b as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn transport_checksum_ipv4(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, segment: &[u8]) -> u16 {
    let mut pseudo = Vec::with_capacity(12 + segment.len());
    pseudo.extend_from_slice(&src.octets());
    pseudo.extend_from_slice(&dst.octets());
    pseudo.push(0);
    pseudo.push(protocol);
    pseudo.extend_from_slice(&(segment.len() as u16).to_be_bytes());
    pseudo.extend_from_slice(segment);
    internet_checksum(&pseudo)
}

pub const DHCP_CLIENT_PORT: u16 = 68;
pub const DHCP_SERVER_PORT: u16 = 67;

pub const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

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
    if payload.len() < 240 {
        return None;
    }
    let op = payload[0];
    if op != 1 && op != 2 {
        return None;
    }
    let htype = payload[1];
    let hlen = payload[2];
    if htype != 1 || hlen != 6 {
        return None;
    }
    let xid = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let yiaddr = Ipv4Addr::new(payload[16], payload[17], payload[18], payload[19]);
    let chaddr = MacAddr([
        payload[28],
        payload[29],
        payload[30],
        payload[31],
        payload[32],
        payload[33],
    ]);
    if payload[236..240] != DHCP_MAGIC_COOKIE {
        return None;
    }
    let mut message_type = None;
    let mut requested_ip = None;
    let mut server_id = None;
    let mut i = 240usize;
    while i < payload.len() {
        let code = payload[i];
        i += 1;
        match code {
            0 => continue, // pad
            255 => break,
            _ => {}
        }
        if i >= payload.len() {
            return None;
        }
        let len = payload[i] as usize;
        i += 1;
        if i + len > payload.len() {
            return None;
        }
        let data = &payload[i..i + len];
        match code {
            53 if len == 1 => {
                message_type = Some(match data[0] {
                    1 => DhcpMessageType::Discover,
                    2 => DhcpMessageType::Offer,
                    3 => DhcpMessageType::Request,
                    5 => DhcpMessageType::Ack,
                    _ => return None,
                });
            }
            50 if len == 4 => {
                requested_ip = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
            }
            54 if len == 4 => {
                server_id = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
            }
            _ => {}
        }
        i += len;
    }
    Some(DhcpParsed {
        xid,
        chaddr,
        yiaddr,
        message_type: message_type?,
        requested_ip,
        server_id,
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
    build_dhcp_reply(
        DhcpMessageType::Offer,
        xid,
        chaddr,
        yiaddr,
        server_ip,
        netmask,
        router,
        dns,
        lease_time_secs,
    )
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
    build_dhcp_reply(
        DhcpMessageType::Ack,
        xid,
        chaddr,
        yiaddr,
        server_ip,
        netmask,
        router,
        dns,
        lease_time_secs,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_dhcp_reply(
    msg_type: DhcpMessageType,
    xid: u32,
    chaddr: MacAddr,
    yiaddr: Ipv4Addr,
    server_ip: Ipv4Addr,
    netmask: Ipv4Addr,
    router: Ipv4Addr,
    dns: Ipv4Addr,
    lease_time_secs: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 2; // BOOTREPLY
    out[1] = 1; // ethernet
    out[2] = 6;
    out[3] = 0;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[16..20].copy_from_slice(&yiaddr.octets());
    out[20..24].copy_from_slice(&server_ip.octets());
    out[28..34].copy_from_slice(chaddr.as_bytes());
    out[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);

    out.extend_from_slice(&[53, 1, msg_type as u8]);
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&server_ip.octets());
    out.extend_from_slice(&[51, 4]);
    out.extend_from_slice(&lease_time_secs.to_be_bytes());
    out.extend_from_slice(&[1, 4]);
    out.extend_from_slice(&netmask.octets());
    out.extend_from_slice(&[3, 4]);
    out.extend_from_slice(&router.octets());
    out.extend_from_slice(&[6, 4]);
    out.extend_from_slice(&dns.octets());
    out.push(255);
    out
}

#[derive(Debug)]
pub struct DnsQuery {
    pub id: u16,
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

pub fn parse_dns_query(packet: &[u8]) -> Option<DnsQuery> {
    if packet.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([packet[0], packet[1]]);
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    if qdcount != 1 {
        return None;
    }
    let mut idx = 12usize;
    let mut labels = Vec::new();
    loop {
        if idx >= packet.len() {
            return None;
        }
        let len = packet[idx] as usize;
        idx += 1;
        if len == 0 {
            break;
        }
        if idx + len > packet.len() {
            return None;
        }
        let label = std::str::from_utf8(&packet[idx..idx + len]).ok()?;
        labels.push(label.to_string());
        idx += len;
    }
    if idx + 4 > packet.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([packet[idx], packet[idx + 1]]);
    let qclass = u16::from_be_bytes([packet[idx + 2], packet[idx + 3]]);
    Some(DnsQuery {
        id,
        name: labels.join("."),
        qtype,
        qclass,
    })
}

pub fn build_dns_response_a(id: u16, query: &DnsQuery, ip: Ipv4Addr) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x8180u16.to_be_bytes()); // standard response, recursion available
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&1u16.to_be_bytes()); // ancount
    out.extend_from_slice(&0u16.to_be_bytes()); // nscount
    out.extend_from_slice(&0u16.to_be_bytes()); // arcount

    // question
    for label in query.name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&query.qtype.to_be_bytes());
    out.extend_from_slice(&query.qclass.to_be_bytes());

    // answer
    out.extend_from_slice(&0xc00cu16.to_be_bytes()); // pointer to name at 12
    out.extend_from_slice(&1u16.to_be_bytes()); // type A
    out.extend_from_slice(&1u16.to_be_bytes()); // class IN
    out.extend_from_slice(&60u32.to_be_bytes()); // ttl
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&ip.octets());
    out
}

pub fn build_dns_response_nxdomain(id: u16, query: &DnsQuery) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x8183u16.to_be_bytes()); // NXDOMAIN
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&0u16.to_be_bytes()); // ancount
    out.extend_from_slice(&0u16.to_be_bytes()); // nscount
    out.extend_from_slice(&0u16.to_be_bytes()); // arcount
    for label in query.name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&query.qtype.to_be_bytes());
    out.extend_from_slice(&query.qclass.to_be_bytes());
    out
}
