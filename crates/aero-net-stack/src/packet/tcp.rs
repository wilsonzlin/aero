#![forbid(unsafe_code)]

use super::ParseError;
use crate::checksum::{ones_complement_finish, ones_complement_sum, pseudo_header_checksum_ipv4};
use core::net::Ipv4Addr;

pub struct TcpFlags;

impl TcpFlags {
    pub const FIN: u8 = 0x01;
    pub const SYN: u8 = 0x02;
    pub const RST: u8 = 0x04;
    pub const PSH: u8 = 0x08;
    pub const ACK: u8 = 0x10;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpSegment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub data_offset: u8,
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent_ptr: u16,
    pub options: &'a [u8],
    pub payload: &'a [u8],
}

impl<'a> TcpSegment<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, ParseError> {
        if buf.len() < 20 {
            return Err(ParseError::Truncated);
        }
        let src_port = u16::from_be_bytes([buf[0], buf[1]]);
        let dst_port = u16::from_be_bytes([buf[2], buf[3]]);
        let seq = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let ack = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let data_offset = buf[12] >> 4;
        let header_len = (data_offset as usize) * 4;
        if data_offset < 5 || buf.len() < header_len {
            return Err(ParseError::Invalid("invalid TCP header"));
        }
        let flags = buf[13];
        let window = u16::from_be_bytes([buf[14], buf[15]]);
        let checksum = u16::from_be_bytes([buf[16], buf[17]]);
        let urgent_ptr = u16::from_be_bytes([buf[18], buf[19]]);
        Ok(Self {
            src_port,
            dst_port,
            seq,
            ack,
            data_offset,
            flags,
            window,
            checksum,
            urgent_ptr,
            options: &buf[20..header_len],
            payload: &buf[header_len..],
        })
    }

    pub fn serialize(
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        seq: u32,
        ack: u32,
        flags: u8,
        window: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let header_len = 20usize;
        let length = header_len + payload.len();
        let mut out = vec![0u8; header_len];
        out[0..2].copy_from_slice(&src_port.to_be_bytes());
        out[2..4].copy_from_slice(&dst_port.to_be_bytes());
        out[4..8].copy_from_slice(&seq.to_be_bytes());
        out[8..12].copy_from_slice(&ack.to_be_bytes());
        out[12] = (5u8 << 4) | 0; // data offset + reserved
        out[13] = flags;
        out[14..16].copy_from_slice(&window.to_be_bytes());
        out[16..18].copy_from_slice(&0u16.to_be_bytes());
        out[18..20].copy_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(payload);

        let mut sum = pseudo_header_checksum_ipv4(src_ip, dst_ip, 6, length as u16);
        sum = ones_complement_sum(sum, &out);
        let checksum = ones_complement_finish(sum);
        out[16..18].copy_from_slice(&checksum.to_be_bytes());
        out
    }
}
