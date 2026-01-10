#![forbid(unsafe_code)]

use super::ParseError;
use crate::checksum::{ones_complement_finish, ones_complement_sum};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IcmpPacket<'a> {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub rest: [u8; 4],
    pub payload: &'a [u8],
}

impl<'a> IcmpPacket<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, ParseError> {
        if buf.len() < 8 {
            return Err(ParseError::Truncated);
        }
        Ok(Self {
            icmp_type: buf[0],
            code: buf[1],
            checksum: u16::from_be_bytes([buf[2], buf[3]]),
            rest: buf[4..8].try_into().unwrap(),
            payload: &buf[8..],
        })
    }

    pub fn serialize(icmp_type: u8, code: u8, rest: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.push(icmp_type);
        out.push(code);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&rest);
        out.extend_from_slice(payload);
        let sum = ones_complement_sum(0, &out);
        let checksum = ones_complement_finish(sum);
        out[2..4].copy_from_slice(&checksum.to_be_bytes());
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IcmpEchoPacket<'a> {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub identifier: u16,
    pub sequence: u16,
    pub payload: &'a [u8],
}

impl<'a> IcmpEchoPacket<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, ParseError> {
        if buf.len() < 8 {
            return Err(ParseError::Truncated);
        }
        Ok(Self {
            icmp_type: buf[0],
            code: buf[1],
            checksum: u16::from_be_bytes([buf[2], buf[3]]),
            identifier: u16::from_be_bytes([buf[4], buf[5]]),
            sequence: u16::from_be_bytes([buf[6], buf[7]]),
            payload: &buf[8..],
        })
    }

    pub fn serialize_echo_reply(identifier: u16, sequence: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.push(0); // Echo reply
        out.push(0); // Code
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&identifier.to_be_bytes());
        out.extend_from_slice(&sequence.to_be_bytes());
        out.extend_from_slice(payload);
        let sum = ones_complement_sum(0, &out);
        let checksum = ones_complement_finish(sum);
        out[2..4].copy_from_slice(&checksum.to_be_bytes());
        out
    }
}
