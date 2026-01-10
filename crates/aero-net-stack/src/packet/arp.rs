#![forbid(unsafe_code)]

use super::{MacAddr, ParseError};
use core::net::Ipv4Addr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArpOperation {
    Request = 1,
    Reply = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArpPacket {
    pub op: ArpOperation,
    pub sender_hw: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_hw: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl ArpPacket {
    pub const LEN: usize = 28;

    pub fn parse(buf: &[u8]) -> Result<Self, ParseError> {
        if buf.len() < Self::LEN {
            return Err(ParseError::Truncated);
        }

        let htype = u16::from_be_bytes([buf[0], buf[1]]);
        let ptype = u16::from_be_bytes([buf[2], buf[3]]);
        let hlen = buf[4];
        let plen = buf[5];
        if htype != 1 || ptype != 0x0800 || hlen != 6 || plen != 4 {
            return Err(ParseError::Invalid("unsupported ARP"));
        }
        let oper = u16::from_be_bytes([buf[6], buf[7]]);
        let op = match oper {
            1 => ArpOperation::Request,
            2 => ArpOperation::Reply,
            _ => return Err(ParseError::Invalid("unknown ARP operation")),
        };

        let sender_hw = MacAddr(buf[8..14].try_into().unwrap());
        let sender_ip = Ipv4Addr::new(buf[14], buf[15], buf[16], buf[17]);
        let target_hw = MacAddr(buf[18..24].try_into().unwrap());
        let target_ip = Ipv4Addr::new(buf[24], buf[25], buf[26], buf[27]);

        Ok(Self {
            op,
            sender_hw,
            sender_ip,
            target_hw,
            target_ip,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::LEN);
        out.extend_from_slice(&1u16.to_be_bytes()); // Ethernet
        out.extend_from_slice(&0x0800u16.to_be_bytes()); // IPv4
        out.push(6);
        out.push(4);
        out.extend_from_slice(&(self.op as u16).to_be_bytes());
        out.extend_from_slice(&self.sender_hw.0);
        out.extend_from_slice(&self.sender_ip.octets());
        out.extend_from_slice(&self.target_hw.0);
        out.extend_from_slice(&self.target_ip.octets());
        out
    }
}
