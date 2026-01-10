//! Ones-complement checksums used by IPv4/UDP/TCP/ICMP.

#![forbid(unsafe_code)]

use core::net::Ipv4Addr;

pub fn ones_complement_sum(mut sum: u32, data: &[u8]) -> u32 {
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let [last] = chunks.remainder() {
        sum += u16::from_be_bytes([*last, 0]) as u32;
    }
    sum
}

pub fn ones_complement_finish(mut sum: u32) -> u16 {
    // Fold carries.
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn ipv4_header_checksum(header: &[u8]) -> u16 {
    debug_assert!(header.len() % 2 == 0);
    let sum = ones_complement_sum(0, header);
    ones_complement_finish(sum)
}

pub fn pseudo_header_checksum_ipv4(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, length: u16) -> u32 {
    let mut sum = 0u32;
    sum = ones_complement_sum(sum, &src.octets());
    sum = ones_complement_sum(sum, &dst.octets());
    sum += protocol as u32;
    sum += length as u32;
    sum
}
