use core::fmt;

pub const VIRTIO_NET_HDR_F_NEEDS_CSUM: u8 = 1;
pub const VIRTIO_NET_HDR_F_DATA_VALID: u8 = 2;

pub const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;
pub const VIRTIO_NET_HDR_GSO_TCPV4: u8 = 1;
pub const VIRTIO_NET_HDR_GSO_UDP: u8 = 3;
pub const VIRTIO_NET_HDR_GSO_TCPV6: u8 = 4;
pub const VIRTIO_NET_HDR_GSO_ECN: u8 = 0x80;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
}

impl VirtioNetHdr {
    pub const LEN: usize = 10;

    pub fn from_bytes_le(bytes: [u8; Self::LEN]) -> Self {
        Self {
            flags: bytes[0],
            gso_type: bytes[1],
            hdr_len: u16::from_le_bytes([bytes[2], bytes[3]]),
            gso_size: u16::from_le_bytes([bytes[4], bytes[5]]),
            csum_start: u16::from_le_bytes([bytes[6], bytes[7]]),
            csum_offset: u16::from_le_bytes([bytes[8], bytes[9]]),
        }
    }

    pub fn needs_csum(self) -> bool {
        self.flags & VIRTIO_NET_HDR_F_NEEDS_CSUM != 0
    }

    pub fn gso_type_base(self) -> u8 {
        self.gso_type & !VIRTIO_NET_HDR_GSO_ECN
    }

    pub fn has_ecn(self) -> bool {
        self.gso_type & VIRTIO_NET_HDR_GSO_ECN != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetOffloadError {
    PacketTooShort,
    ChecksumOffsetOutOfBounds,
    UnsupportedEthertype(u16),
    UnsupportedIpVersion(u8),
    UnsupportedL4Protocol(u8),
    UnsupportedGsoType(u8),
    InvalidHdrLen { expected: usize, actual: usize },
    InvalidGsoSize,
}

impl fmt::Display for NetOffloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketTooShort => write!(f, "packet too short"),
            Self::ChecksumOffsetOutOfBounds => write!(f, "checksum offsets out of bounds"),
            Self::UnsupportedEthertype(et) => write!(f, "unsupported ethertype 0x{et:04x}"),
            Self::UnsupportedIpVersion(v) => write!(f, "unsupported IP version {v}"),
            Self::UnsupportedL4Protocol(p) => write!(f, "unsupported L4 protocol {p}"),
            Self::UnsupportedGsoType(t) => write!(f, "unsupported GSO type {t}"),
            Self::InvalidHdrLen { expected, actual } => {
                write!(
                    f,
                    "virtio_net_hdr hdr_len mismatch: expected {expected} actual {actual}"
                )
            }
            Self::InvalidGsoSize => write!(f, "invalid gso_size"),
        }
    }
}

impl std::error::Error for NetOffloadError {}

pub fn process_tx_packet(
    hdr: VirtioNetHdr,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, NetOffloadError> {
    let gso_type = hdr.gso_type_base();
    if gso_type != VIRTIO_NET_HDR_GSO_NONE {
        return segment_gso_packet(hdr, packet);
    }

    let mut out = packet.to_vec();
    if hdr.needs_csum() {
        apply_partial_checksum_offload(&hdr, &mut out)?;
    }
    Ok(vec![out])
}

fn apply_partial_checksum_offload(
    hdr: &VirtioNetHdr,
    packet: &mut [u8],
) -> Result<(), NetOffloadError> {
    let start = hdr.csum_start as usize;
    let field = start
        .checked_add(hdr.csum_offset as usize)
        .ok_or(NetOffloadError::ChecksumOffsetOutOfBounds)?;

    if start > packet.len() || field + 2 > packet.len() {
        return Err(NetOffloadError::ChecksumOffsetOutOfBounds);
    }

    let checksum = ones_complement_checksum(&packet[start..]);
    packet[field..field + 2].copy_from_slice(&checksum.to_be_bytes());
    Ok(())
}

fn segment_gso_packet(hdr: VirtioNetHdr, packet: &[u8]) -> Result<Vec<Vec<u8>>, NetOffloadError> {
    if hdr.gso_size == 0 {
        return Err(NetOffloadError::InvalidGsoSize);
    }

    match hdr.gso_type_base() {
        VIRTIO_NET_HDR_GSO_TCPV4 => segment_tcpv4(hdr, packet),
        VIRTIO_NET_HDR_GSO_TCPV6 => segment_tcpv6(hdr, packet),
        other => Err(NetOffloadError::UnsupportedGsoType(other)),
    }
}

const ETH_HEADER_LEN: usize = 14;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_VLAN: u16 = 0x8100;
const ETHERTYPE_QINQ: u16 = 0x88A8;
const ETHERTYPE_IPV6: u16 = 0x86DD;

#[derive(Debug, Clone, Copy)]
struct EthernetFrame {
    l2_len: usize,
    ethertype: u16,
}

fn parse_ethernet(packet: &[u8]) -> Result<EthernetFrame, NetOffloadError> {
    if packet.len() < ETH_HEADER_LEN {
        return Err(NetOffloadError::PacketTooShort);
    }
    let mut l2_len = ETH_HEADER_LEN;
    let mut ethertype = u16::from_be_bytes([packet[12], packet[13]]);

    if ethertype == ETHERTYPE_VLAN || ethertype == ETHERTYPE_QINQ {
        if packet.len() < ETH_HEADER_LEN + 4 {
            return Err(NetOffloadError::PacketTooShort);
        }
        ethertype = u16::from_be_bytes([packet[16], packet[17]]);
        l2_len += 4;
    }

    Ok(EthernetFrame { l2_len, ethertype })
}

fn segment_tcpv4(hdr: VirtioNetHdr, packet: &[u8]) -> Result<Vec<Vec<u8>>, NetOffloadError> {
    let eth = parse_ethernet(packet)?;
    if eth.ethertype != ETHERTYPE_IPV4 {
        return Err(NetOffloadError::UnsupportedEthertype(eth.ethertype));
    }

    let l3_offset = eth.l2_len;
    let ipv4 = parse_ipv4(&packet[l3_offset..])?;
    if ipv4.protocol != 6 {
        return Err(NetOffloadError::UnsupportedL4Protocol(ipv4.protocol));
    }

    let l4_offset = l3_offset + ipv4.header_len;
    let tcp = parse_tcp(&packet[l4_offset..])?;

    let headers_len = l4_offset + tcp.header_len;
    if hdr.hdr_len != 0 && headers_len != hdr.hdr_len as usize {
        return Err(NetOffloadError::InvalidHdrLen {
            expected: headers_len,
            actual: hdr.hdr_len as usize,
        });
    }

    let payload = &packet[headers_len..];
    let gso_size = hdr.gso_size as usize;
    let total_segments = payload.chunks(gso_size).len();

    let mut segments = Vec::with_capacity(total_segments);
    let mut seq = tcp.seq;
    let base_ip_id = ipv4.identification;

    for (i, chunk) in payload.chunks(gso_size).enumerate() {
        let mut seg = Vec::with_capacity(headers_len + chunk.len());
        seg.extend_from_slice(&packet[..headers_len]);
        seg.extend_from_slice(chunk);

        // Update IPv4 total length and identification.
        let seg_ip_total_len = (ipv4.header_len + tcp.header_len + chunk.len()) as u16;
        seg[l3_offset + 2..l3_offset + 4].copy_from_slice(&seg_ip_total_len.to_be_bytes());
        let ip_id = base_ip_id.wrapping_add(i as u16);
        seg[l3_offset + 4..l3_offset + 6].copy_from_slice(&ip_id.to_be_bytes());

        // Recompute IPv4 header checksum.
        seg[l3_offset + 10..l3_offset + 12].copy_from_slice(&0u16.to_be_bytes());
        let ip_csum = ones_complement_checksum(&seg[l3_offset..l3_offset + ipv4.header_len]);
        seg[l3_offset + 10..l3_offset + 12].copy_from_slice(&ip_csum.to_be_bytes());

        // Update TCP sequence number.
        seg[l4_offset + 4..l4_offset + 8].copy_from_slice(&seq.to_be_bytes());

        // Clear FIN/PSH for non-last segments.
        let is_last = i + 1 == total_segments;
        if !is_last {
            seg[l4_offset + 13] &= !(0x01 | 0x08);
        }

        // Recompute TCP checksum (includes pseudo-header).
        seg[l4_offset + 16..l4_offset + 18].copy_from_slice(&0u16.to_be_bytes());
        let tcp_len = (tcp.header_len + chunk.len()) as u16;
        let tcp_csum = tcp_checksum_ipv4(
            &ipv4.src,
            &ipv4.dst,
            &seg[l4_offset..l4_offset + tcp.header_len + chunk.len()],
            tcp_len,
        );
        seg[l4_offset + 16..l4_offset + 18].copy_from_slice(&tcp_csum.to_be_bytes());

        segments.push(seg);
        seq = seq.wrapping_add(chunk.len() as u32);
    }

    Ok(segments)
}

fn segment_tcpv6(hdr: VirtioNetHdr, packet: &[u8]) -> Result<Vec<Vec<u8>>, NetOffloadError> {
    let eth = parse_ethernet(packet)?;
    if eth.ethertype != ETHERTYPE_IPV6 {
        return Err(NetOffloadError::UnsupportedEthertype(eth.ethertype));
    }

    let l3_offset = eth.l2_len;
    let ipv6 = parse_ipv6(&packet[l3_offset..])?;
    if ipv6.next_header != 6 {
        return Err(NetOffloadError::UnsupportedL4Protocol(ipv6.next_header));
    }

    let l4_offset = l3_offset + ipv6.header_len;
    let tcp = parse_tcp(&packet[l4_offset..])?;

    let headers_len = l4_offset + tcp.header_len;
    if hdr.hdr_len != 0 && headers_len != hdr.hdr_len as usize {
        return Err(NetOffloadError::InvalidHdrLen {
            expected: headers_len,
            actual: hdr.hdr_len as usize,
        });
    }

    let payload = &packet[headers_len..];
    let gso_size = hdr.gso_size as usize;

    let total_segments = payload.chunks(gso_size).len();
    let mut segments = Vec::with_capacity(total_segments);
    let mut seq = tcp.seq;

    for (i, chunk) in payload.chunks(gso_size).enumerate() {
        let mut seg = Vec::with_capacity(headers_len + chunk.len());
        seg.extend_from_slice(&packet[..headers_len]);
        seg.extend_from_slice(chunk);

        // Update IPv6 payload length (excludes IPv6 header).
        let seg_payload_len = (tcp.header_len + chunk.len()) as u16;
        seg[l3_offset + 4..l3_offset + 6].copy_from_slice(&seg_payload_len.to_be_bytes());

        // Update TCP sequence number.
        seg[l4_offset + 4..l4_offset + 8].copy_from_slice(&seq.to_be_bytes());

        // Clear FIN/PSH for non-last segments.
        let is_last = i + 1 == total_segments;
        if !is_last {
            seg[l4_offset + 13] &= !(0x01 | 0x08);
        }

        // Recompute TCP checksum (includes pseudo-header).
        seg[l4_offset + 16..l4_offset + 18].copy_from_slice(&0u16.to_be_bytes());
        let tcp_csum = tcp_checksum_ipv6(
            &ipv6.src,
            &ipv6.dst,
            &seg[l4_offset..l4_offset + tcp.header_len + chunk.len()],
            (tcp.header_len + chunk.len()) as u32,
        );
        seg[l4_offset + 16..l4_offset + 18].copy_from_slice(&tcp_csum.to_be_bytes());

        segments.push(seg);
        seq = seq.wrapping_add(chunk.len() as u32);
    }

    Ok(segments)
}

#[derive(Debug, Clone, Copy)]
struct Ipv4Header {
    header_len: usize,
    protocol: u8,
    src: [u8; 4],
    dst: [u8; 4],
    identification: u16,
}

fn parse_ipv4(buf: &[u8]) -> Result<Ipv4Header, NetOffloadError> {
    if buf.len() < 20 {
        return Err(NetOffloadError::PacketTooShort);
    }
    let version = buf[0] >> 4;
    if version != 4 {
        return Err(NetOffloadError::UnsupportedIpVersion(version));
    }
    let ihl = (buf[0] & 0x0F) as usize * 4;
    if ihl < 20 || buf.len() < ihl {
        return Err(NetOffloadError::PacketTooShort);
    }
    Ok(Ipv4Header {
        header_len: ihl,
        protocol: buf[9],
        src: [buf[12], buf[13], buf[14], buf[15]],
        dst: [buf[16], buf[17], buf[18], buf[19]],
        identification: u16::from_be_bytes([buf[4], buf[5]]),
    })
}

#[derive(Debug, Clone, Copy)]
struct Ipv6Header {
    header_len: usize,
    next_header: u8,
    src: [u8; 16],
    dst: [u8; 16],
}

fn parse_ipv6(buf: &[u8]) -> Result<Ipv6Header, NetOffloadError> {
    if buf.len() < 40 {
        return Err(NetOffloadError::PacketTooShort);
    }
    let version = buf[0] >> 4;
    if version != 6 {
        return Err(NetOffloadError::UnsupportedIpVersion(version));
    }
    let mut src = [0u8; 16];
    let mut dst = [0u8; 16];
    src.copy_from_slice(&buf[8..24]);
    dst.copy_from_slice(&buf[24..40]);
    Ok(Ipv6Header {
        header_len: 40,
        next_header: buf[6],
        src,
        dst,
    })
}

#[derive(Debug, Clone, Copy)]
struct TcpHeader {
    header_len: usize,
    seq: u32,
}

fn parse_tcp(buf: &[u8]) -> Result<TcpHeader, NetOffloadError> {
    if buf.len() < 20 {
        return Err(NetOffloadError::PacketTooShort);
    }
    let data_offset = (buf[12] >> 4) as usize * 4;
    if data_offset < 20 || buf.len() < data_offset {
        return Err(NetOffloadError::PacketTooShort);
    }
    Ok(TcpHeader {
        header_len: data_offset,
        seq: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
    })
}

fn ones_complement_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += (last as u32) << 8;
    }
    fold_checksum_sum(sum)
}

fn fold_checksum_sum(mut sum: u32) -> u16 {
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn checksum_sum_u16_words(data: &[u8], mut sum: u32) -> u32 {
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += (last as u32) << 8;
    }
    sum
}

fn tcp_checksum_ipv4(src: &[u8; 4], dst: &[u8; 4], tcp_segment: &[u8], tcp_len: u16) -> u16 {
    let mut sum: u32 = 0;
    sum = checksum_sum_u16_words(src, sum);
    sum = checksum_sum_u16_words(dst, sum);
    sum += 6u32;
    sum += tcp_len as u32;
    sum = checksum_sum_u16_words(tcp_segment, sum);
    fold_checksum_sum(sum)
}

fn tcp_checksum_ipv6(src: &[u8; 16], dst: &[u8; 16], tcp_segment: &[u8], tcp_len: u32) -> u16 {
    let mut sum: u32 = 0;
    sum = checksum_sum_u16_words(src, sum);
    sum = checksum_sum_u16_words(dst, sum);
    sum += (tcp_len >> 16) as u32;
    sum += (tcp_len & 0xFFFF) as u32;
    sum += 6u32;
    sum = checksum_sum_u16_words(tcp_segment, sum);
    fold_checksum_sum(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn udp_pseudo_header_sum_ipv4(src: [u8; 4], dst: [u8; 4], udp_len: u16) -> u16 {
        let mut sum: u32 = 0;
        sum = checksum_sum_u16_words(&src, sum);
        sum = checksum_sum_u16_words(&dst, sum);
        sum += 17u32;
        sum += udp_len as u32;

        while (sum >> 16) != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        sum as u16
    }

    fn build_ipv4_udp_frame(payload: &[u8]) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        packet.extend_from_slice(&[0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb]);
        packet.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());

        let total_len = (20 + 8 + payload.len()) as u16;
        let mut ipv4 = [0u8; 20];
        ipv4[0] = (4 << 4) | 5;
        ipv4[2..4].copy_from_slice(&total_len.to_be_bytes());
        ipv4[8] = 64;
        ipv4[9] = 17;
        ipv4[12..16].copy_from_slice(&[192, 0, 2, 1]);
        ipv4[16..20].copy_from_slice(&[198, 51, 100, 2]);
        ipv4[10..12].copy_from_slice(&0u16.to_be_bytes());
        let ip_csum = ones_complement_checksum(&ipv4);
        ipv4[10..12].copy_from_slice(&ip_csum.to_be_bytes());
        packet.extend_from_slice(&ipv4);

        let udp_len = (8 + payload.len()) as u16;
        let mut udp = [0u8; 8];
        udp[0..2].copy_from_slice(&1234u16.to_be_bytes());
        udp[2..4].copy_from_slice(&5678u16.to_be_bytes());
        udp[4..6].copy_from_slice(&udp_len.to_be_bytes());
        packet.extend_from_slice(&udp);
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn tx_checksum_offload_fills_udp_checksum() {
        let payload = b"hello world";
        let mut packet = build_ipv4_udp_frame(payload);

        let eth_off = ETH_HEADER_LEN;
        let udp_off = eth_off + 20;
        let udp_len = (8 + payload.len()) as u16;

        let pseudo = udp_pseudo_header_sum_ipv4([192, 0, 2, 1], [198, 51, 100, 2], udp_len);
        packet[udp_off + 6..udp_off + 8].copy_from_slice(&pseudo.to_be_bytes());

        let hdr = VirtioNetHdr {
            flags: VIRTIO_NET_HDR_F_NEEDS_CSUM,
            gso_type: VIRTIO_NET_HDR_GSO_NONE,
            hdr_len: 0,
            gso_size: 0,
            csum_start: udp_off as u16,
            csum_offset: 6,
        };

        let processed = process_tx_packet(hdr, &packet).unwrap();
        assert_eq!(processed.len(), 1);
        let out = &processed[0];

        let mut udp_segment = out[udp_off..udp_off + udp_len as usize].to_vec();
        udp_segment[6..8].copy_from_slice(&0u16.to_be_bytes());

        let mut sum: u32 = 0;
        sum = checksum_sum_u16_words(&[192, 0, 2, 1], sum);
        sum = checksum_sum_u16_words(&[198, 51, 100, 2], sum);
        sum += 17u32;
        sum += udp_len as u32;
        sum = checksum_sum_u16_words(&udp_segment, sum);
        let expected = fold_checksum_sum(sum);

        let actual = u16::from_be_bytes([out[udp_off + 6], out[udp_off + 7]]);
        assert_eq!(actual, expected);
    }

    fn build_ipv4_tcp_frame(payload_len: usize, flags: u8) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        packet.extend_from_slice(&[0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb]);
        packet.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());

        let total_len = (20 + 20 + payload_len) as u16;
        let identification = 0x1111u16;
        let mut ipv4 = [0u8; 20];
        ipv4[0] = (4 << 4) | 5;
        ipv4[2..4].copy_from_slice(&total_len.to_be_bytes());
        ipv4[4..6].copy_from_slice(&identification.to_be_bytes());
        ipv4[8] = 64;
        ipv4[9] = 6;
        ipv4[12..16].copy_from_slice(&[10, 0, 0, 1]);
        ipv4[16..20].copy_from_slice(&[10, 0, 0, 2]);
        ipv4[10..12].copy_from_slice(&0u16.to_be_bytes());
        let ip_csum = ones_complement_checksum(&ipv4);
        ipv4[10..12].copy_from_slice(&ip_csum.to_be_bytes());
        packet.extend_from_slice(&ipv4);

        let seq = 0x01020304u32;
        let ack = 0u32;
        let mut tcp = [0u8; 20];
        tcp[0..2].copy_from_slice(&1000u16.to_be_bytes());
        tcp[2..4].copy_from_slice(&2000u16.to_be_bytes());
        tcp[4..8].copy_from_slice(&seq.to_be_bytes());
        tcp[8..12].copy_from_slice(&ack.to_be_bytes());
        tcp[12] = 5u8 << 4;
        tcp[13] = flags;
        tcp[14..16].copy_from_slice(&4096u16.to_be_bytes());
        packet.extend_from_slice(&tcp);

        packet.extend(std::iter::repeat(0x42u8).take(payload_len));
        packet
    }

    #[test]
    fn tx_gso_tcpv4_segments_and_updates_headers() {
        let payload_len = 3000;
        let mss = 1000usize;
        let flags = 0x18; // PSH|ACK
        let packet = build_ipv4_tcp_frame(payload_len, flags);

        let hdr = VirtioNetHdr {
            flags: VIRTIO_NET_HDR_F_NEEDS_CSUM,
            gso_type: VIRTIO_NET_HDR_GSO_TCPV4,
            hdr_len: (ETH_HEADER_LEN + 20 + 20) as u16,
            gso_size: mss as u16,
            csum_start: 0,
            csum_offset: 0,
        };

        let segments = process_tx_packet(hdr, &packet).unwrap();
        assert_eq!(segments.len(), 3);

        let base_seq = 0x01020304u32;
        let ip_offset = ETH_HEADER_LEN;
        let tcp_offset = ip_offset + 20;

        for (i, seg) in segments.iter().enumerate() {
            let seg_payload_len = if i < 2 { mss } else { payload_len - 2 * mss };
            assert_eq!(seg.len(), ETH_HEADER_LEN + 20 + 20 + seg_payload_len);

            let total_len = u16::from_be_bytes([seg[ip_offset + 2], seg[ip_offset + 3]]);
            assert_eq!(total_len, (20 + 20 + seg_payload_len) as u16);

            let ip_id = u16::from_be_bytes([seg[ip_offset + 4], seg[ip_offset + 5]]);
            assert_eq!(ip_id, 0x1111u16.wrapping_add(i as u16));

            let seq = u32::from_be_bytes([
                seg[tcp_offset + 4],
                seg[tcp_offset + 5],
                seg[tcp_offset + 6],
                seg[tcp_offset + 7],
            ]);
            assert_eq!(seq, base_seq + (i * mss) as u32);

            let flags_out = seg[tcp_offset + 13];
            if i < 2 {
                assert_eq!(flags_out, 0x10);
            } else {
                assert_eq!(flags_out, flags);
            }

            let ip_header = &seg[ip_offset..ip_offset + 20];
            assert_eq!(ones_complement_checksum(ip_header), 0);

            let tcp_len = (20 + seg_payload_len) as u16;
            let tcp_segment = &seg[tcp_offset..tcp_offset + 20 + seg_payload_len];
            let tcp_csum = tcp_checksum_ipv4(&[10, 0, 0, 1], &[10, 0, 0, 2], tcp_segment, tcp_len);
            assert_eq!(tcp_csum, 0);
        }
    }
}
