use std::net::Ipv4Addr;

use super::ethernet::{build_ethernet_frame, EtherType, EthernetFrame, MacAddr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArpOp {
    Request = 1,
    Reply = 2,
}

#[derive(Debug, Clone, Copy)]
pub struct ArpPacket {
    pub op: ArpOp,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl ArpPacket {
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 28 {
            return None;
        }
        let htype = u16::from_be_bytes([buf[0], buf[1]]);
        let ptype = u16::from_be_bytes([buf[2], buf[3]]);
        let hlen = buf[4];
        let plen = buf[5];
        if htype != 1 || ptype != 0x0800 || hlen != 6 || plen != 4 {
            return None;
        }
        let op_raw = u16::from_be_bytes([buf[6], buf[7]]);
        let op = match op_raw {
            1 => ArpOp::Request,
            2 => ArpOp::Reply,
            _ => return None,
        };
        let sender_mac = MacAddr::new(buf[8..14].try_into().ok()?);
        let sender_ip = Ipv4Addr::new(buf[14], buf[15], buf[16], buf[17]);
        let target_mac = MacAddr::new(buf[18..24].try_into().ok()?);
        let target_ip = Ipv4Addr::new(buf[24], buf[25], buf[26], buf[27]);
        Some(Self {
            op,
            sender_mac,
            sender_ip,
            target_mac,
            target_ip,
        })
    }

    pub fn serialize(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(28);
        out.extend_from_slice(&1u16.to_be_bytes()); // Ethernet
        out.extend_from_slice(&0x0800u16.to_be_bytes()); // IPv4
        out.push(6);
        out.push(4);
        out.extend_from_slice(&(self.op as u16).to_be_bytes());
        out.extend_from_slice(&self.sender_mac.octets());
        out.extend_from_slice(&self.sender_ip.octets());
        out.extend_from_slice(&self.target_mac.octets());
        out.extend_from_slice(&self.target_ip.octets());
        out
    }
}

#[derive(Debug)]
pub struct ArpResponder {
    gateway_mac: MacAddr,
    gateway_ip: Ipv4Addr,
}

impl ArpResponder {
    pub fn new(gateway_mac: MacAddr, gateway_ip: Ipv4Addr) -> Self {
        Self {
            gateway_mac,
            gateway_ip,
        }
    }

    pub fn handle(&mut self, eth: &EthernetFrame<'_>) -> Option<Vec<u8>> {
        let arp = ArpPacket::parse(eth.payload)?;

        if arp.op != ArpOp::Request || arp.target_ip != self.gateway_ip {
            return None;
        }

        let reply = ArpPacket {
            op: ArpOp::Reply,
            sender_mac: self.gateway_mac,
            sender_ip: self.gateway_ip,
            target_mac: arp.sender_mac,
            target_ip: arp.sender_ip,
        };

        Some(build_ethernet_frame(
            arp.sender_mac,
            self.gateway_mac,
            EtherType::Arp,
            &reply.serialize(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arp_request_reply_round_trip() {
        let gateway_mac = MacAddr::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
        let guest_mac = MacAddr::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        let guest_ip = Ipv4Addr::new(10, 0, 2, 15);

        let req = ArpPacket {
            op: ArpOp::Request,
            sender_mac: guest_mac,
            sender_ip: guest_ip,
            target_mac: MacAddr::new([0, 0, 0, 0, 0, 0]),
            target_ip: gateway_ip,
        };
        let frame = build_ethernet_frame(
            MacAddr::BROADCAST,
            guest_mac,
            EtherType::Arp,
            &req.serialize(),
        );
        let eth = EthernetFrame::parse(&frame).unwrap();

        let mut responder = ArpResponder::new(gateway_mac, gateway_ip);
        let reply_frame = responder.handle(&eth).unwrap();

        let reply_eth = EthernetFrame::parse(&reply_frame).unwrap();
        assert_eq!(reply_eth.dst, guest_mac);
        assert_eq!(reply_eth.src, gateway_mac);
        assert_eq!(reply_eth.ethertype, EtherType::Arp);

        let reply = ArpPacket::parse(reply_eth.payload).unwrap();
        assert_eq!(reply.op, ArpOp::Reply);
        assert_eq!(reply.sender_mac, gateway_mac);
        assert_eq!(reply.sender_ip, gateway_ip);
        assert_eq!(reply.target_mac, guest_mac);
        assert_eq!(reply.target_ip, guest_ip);
    }
}

