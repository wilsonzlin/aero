#![forbid(unsafe_code)]

use super::ParseError;
use crate::checksum::ipv4_header_checksum;
use core::net::Ipv4Addr;

pub struct Ipv4Protocol;

impl Ipv4Protocol {
    pub const ICMP: u8 = 1;
    pub const TCP: u8 = 6;
    pub const UDP: u8 = 17;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Packet<'a> {
    pub dscp_ecn: u8,
    pub total_len: u16,
    pub identification: u16,
    pub flags_fragment: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub header_checksum: u16,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub options: &'a [u8],
    pub payload: &'a [u8],
}

impl<'a> Ipv4Packet<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, ParseError> {
        if buf.len() < 20 {
            return Err(ParseError::Truncated);
        }
        let version = buf[0] >> 4;
        let ihl = (buf[0] & 0x0f) as usize;
        if version != 4 || ihl < 5 {
            return Err(ParseError::Invalid("invalid IPv4 header"));
        }
        let header_len = ihl * 4;
        if buf.len() < header_len {
            return Err(ParseError::Truncated);
        }
        let total_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        if total_len < header_len || buf.len() < total_len {
            return Err(ParseError::Truncated);
        }

        let dscp_ecn = buf[1];
        let identification = u16::from_be_bytes([buf[4], buf[5]]);
        let flags_fragment = u16::from_be_bytes([buf[6], buf[7]]);
        let ttl = buf[8];
        let protocol = buf[9];
        let header_checksum = u16::from_be_bytes([buf[10], buf[11]]);
        let src = Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]);
        let dst = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);

        let options = &buf[20..header_len];
        let payload = &buf[header_len..total_len];

        Ok(Self {
            dscp_ecn,
            total_len: total_len as u16,
            identification,
            flags_fragment,
            ttl,
            protocol,
            header_checksum,
            src,
            dst,
            options,
            payload,
        })
    }

    pub fn serialize(
        src: Ipv4Addr,
        dst: Ipv4Addr,
        protocol: u8,
        identification: u16,
        ttl: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let header_len = 20usize;
        let total_len = header_len + payload.len();
        let mut out = vec![0u8; header_len];
        out[0] = (4u8 << 4) | 5; // version + IHL
        out[1] = 0; // DSCP/ECN
        out[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        out[4..6].copy_from_slice(&identification.to_be_bytes());
        out[6..8].copy_from_slice(&0x4000u16.to_be_bytes()); // DF
        out[8] = ttl;
        out[9] = protocol;
        out[10..12].copy_from_slice(&0u16.to_be_bytes());
        out[12..16].copy_from_slice(&src.octets());
        out[16..20].copy_from_slice(&dst.octets());
        let csum = ipv4_header_checksum(&out);
        out[10..12].copy_from_slice(&csum.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }
}
