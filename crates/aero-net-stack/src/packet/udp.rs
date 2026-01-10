#![forbid(unsafe_code)]

use super::ParseError;
use crate::checksum::{ones_complement_finish, ones_complement_sum, pseudo_header_checksum_ipv4};
use core::net::Ipv4Addr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpDatagram<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub length: u16,
    pub checksum: u16,
    pub payload: &'a [u8],
}

impl<'a> UdpDatagram<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, ParseError> {
        if buf.len() < 8 {
            return Err(ParseError::Truncated);
        }
        let src_port = u16::from_be_bytes([buf[0], buf[1]]);
        let dst_port = u16::from_be_bytes([buf[2], buf[3]]);
        let length = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        let checksum = u16::from_be_bytes([buf[6], buf[7]]);
        if length < 8 || buf.len() < length {
            return Err(ParseError::Truncated);
        }
        Ok(Self {
            src_port,
            dst_port,
            length: length as u16,
            checksum,
            payload: &buf[8..length],
        })
    }

    pub fn serialize(
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let length = 8 + payload.len();
        let mut out = vec![0u8; 8];
        out[0..2].copy_from_slice(&src_port.to_be_bytes());
        out[2..4].copy_from_slice(&dst_port.to_be_bytes());
        out[4..6].copy_from_slice(&(length as u16).to_be_bytes());
        out[6..8].copy_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(payload);

        let mut sum = pseudo_header_checksum_ipv4(src_ip, dst_ip, 17, length as u16);
        sum = ones_complement_sum(sum, &out);
        let checksum = ones_complement_finish(sum);
        out[6..8].copy_from_slice(&checksum.to_be_bytes());
        out
    }
}
