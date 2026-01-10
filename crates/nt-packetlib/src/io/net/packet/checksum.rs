//! Internet checksum helpers (RFC 1071).

use core::net::Ipv4Addr;

/// Add a slice of bytes interpreted as big-endian 16-bit words to a running sum.
///
/// This is the standard accumulator used for IPv4/TCP/UDP/ICMP checksums.
pub fn ones_complement_add(mut sum: u32, bytes: &[u8]) -> u32 {
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }

    let rem = chunks.remainder();
    if let [last] = rem {
        sum = sum.wrapping_add((*last as u32) << 8);
    }

    sum
}

/// Fold a 32-bit sum into 16 bits using end-around carry.
pub fn ones_complement_fold(mut sum: u32) -> u16 {
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    sum as u16
}

/// Finalize an accumulated one's-complement sum into a checksum field value.
pub fn ones_complement_finalize(sum: u32) -> u16 {
    !ones_complement_fold(sum)
}

/// Compute the Internet checksum over a single byte slice.
pub fn internet_checksum(bytes: &[u8]) -> u16 {
    ones_complement_finalize(ones_complement_add(0, bytes))
}

/// Compute an IPv4 header checksum.
///
/// Callers must ensure the checksum field in the header is set to zero before calling.
pub fn ipv4_header_checksum(header: &[u8]) -> u16 {
    internet_checksum(header)
}

/// Compute a TCP/UDP checksum over an IPv4 pseudo-header and the transport segment.
///
/// Callers must ensure the checksum field inside `segment` is set to zero before calling.
pub fn transport_checksum_ipv4(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    protocol: u8,
    segment: &[u8],
) -> u16 {
    let mut sum = 0u32;
    sum = ones_complement_add(sum, &src_ip.octets());
    sum = ones_complement_add(sum, &dst_ip.octets());
    sum = ones_complement_add(sum, &[0, protocol]);
    sum = ones_complement_add(sum, &(segment.len() as u16).to_be_bytes());
    sum = ones_complement_add(sum, segment);
    ones_complement_finalize(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_header_checksum_rfc1071_example() {
        // Example commonly attributed to RFC 1071.
        // Header bytes with checksum set to 0x0000.
        let mut hdr = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0,
            0xa8, 0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        let csum = ipv4_header_checksum(&hdr);
        assert_eq!(csum, 0xb861);
        hdr[10..12].copy_from_slice(&csum.to_be_bytes());
        assert_eq!(internet_checksum(&hdr), 0);
    }

    #[test]
    fn transport_checksum_ipv4_known_vector_udp() {
        // Vector computed independently (Python reference implementation):
        // src=10.0.0.1 dst=10.0.0.2 UDP 1234->53 payload="hello" -> 0xa2f8
        let src = Ipv4Addr::new(10, 0, 0, 1);
        let dst = Ipv4Addr::new(10, 0, 0, 2);
        let mut seg = [
            0x04, 0xd2, 0x00, 0x35, 0x00, 0x0d, 0x00, 0x00, b'h', b'e', b'l', b'l', b'o',
        ];
        let csum = transport_checksum_ipv4(src, dst, 17, &seg);
        assert_eq!(csum, 0xa2f8);
        seg[6..8].copy_from_slice(&csum.to_be_bytes());
        assert_eq!(transport_checksum_ipv4(src, dst, 17, &seg), 0);
    }

    #[test]
    fn transport_checksum_ipv4_known_vector_tcp() {
        // Vector computed independently (Python reference implementation):
        // src=192.0.2.1 dst=192.0.2.2 TCP 1000->80 seq=1 ack=0 flags=SYN window=1024 -> 0x23a6
        let src = Ipv4Addr::new(192, 0, 2, 1);
        let dst = Ipv4Addr::new(192, 0, 2, 2);
        let mut seg = [
            0x03, 0xe8, 0x00, 0x50, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x50,
            0x02, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let csum = transport_checksum_ipv4(src, dst, 6, &seg);
        assert_eq!(csum, 0x23a6);
        seg[16..18].copy_from_slice(&csum.to_be_bytes());
        assert_eq!(transport_checksum_ipv4(src, dst, 6, &seg), 0);
    }
}
