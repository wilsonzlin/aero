#![forbid(unsafe_code)]

use super::ParseError;
use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: Self = Self([0xff; 6]);
}

impl fmt::Debug for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

pub struct EtherType;

impl EtherType {
    pub const IPV4: u16 = 0x0800;
    pub const ARP: u16 = 0x0806;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetFrame<'a> {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

impl<'a> EthernetFrame<'a> {
    pub const HEADER_LEN: usize = 14;

    pub fn parse(buf: &'a [u8]) -> Result<Self, ParseError> {
        if buf.len() < Self::HEADER_LEN {
            return Err(ParseError::Truncated);
        }
        let dst = MacAddr(buf[0..6].try_into().unwrap());
        let src = MacAddr(buf[6..12].try_into().unwrap());
        let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
        Ok(Self {
            dst,
            src,
            ethertype,
            payload: &buf[Self::HEADER_LEN..],
        })
    }

    pub fn serialize(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::HEADER_LEN + payload.len());
        out.extend_from_slice(&dst.0);
        out.extend_from_slice(&src.0);
        out.extend_from_slice(&ethertype.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }
}
