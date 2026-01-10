use core::net::Ipv4Addr;

use super::{ensure_len, ensure_out_buf_len, MacAddr, PacketError};

pub const HTYPE_ETHERNET: u16 = 1;
pub const PTYPE_IPV4: u16 = 0x0800;

pub const ARP_OP_REQUEST: u16 = 1;
pub const ARP_OP_REPLY: u16 = 2;

#[derive(Clone, Copy, Debug)]
pub struct ArpPacket<'a> {
    data: &'a [u8],
    hlen: usize,
    plen: usize,
}

impl<'a> ArpPacket<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, PacketError> {
        ensure_len(data, 8)?;
        let hlen = data[4] as usize;
        let plen = data[5] as usize;
        let total = 8 + 2 * hlen + 2 * plen;
        ensure_len(data, total)?;
        Ok(Self { data, hlen, plen })
    }

    pub fn htype(&self) -> u16 {
        u16::from_be_bytes([self.data[0], self.data[1]])
    }

    pub fn ptype(&self) -> u16 {
        u16::from_be_bytes([self.data[2], self.data[3]])
    }

    pub fn hlen(&self) -> u8 {
        self.hlen as u8
    }

    pub fn plen(&self) -> u8 {
        self.plen as u8
    }

    pub fn opcode(&self) -> u16 {
        u16::from_be_bytes([self.data[6], self.data[7]])
    }

    fn addr_offsets(&self) -> (usize, usize, usize, usize) {
        let sha = 8;
        let spa = sha + self.hlen;
        let tha = spa + self.plen;
        let tpa = tha + self.hlen;
        (sha, spa, tha, tpa)
    }

    pub fn sender_hw_addr(&self) -> &'a [u8] {
        let (sha, _, _, _) = self.addr_offsets();
        &self.data[sha..sha + self.hlen]
    }

    pub fn sender_proto_addr(&self) -> &'a [u8] {
        let (_, spa, _, _) = self.addr_offsets();
        &self.data[spa..spa + self.plen]
    }

    pub fn target_hw_addr(&self) -> &'a [u8] {
        let (_, _, tha, _) = self.addr_offsets();
        &self.data[tha..tha + self.hlen]
    }

    pub fn target_proto_addr(&self) -> &'a [u8] {
        let (_, _, _, tpa) = self.addr_offsets();
        &self.data[tpa..tpa + self.plen]
    }

    pub fn sender_mac(&self) -> Option<MacAddr> {
        if self.hlen == 6 {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(self.sender_hw_addr());
            Some(MacAddr(mac))
        } else {
            None
        }
    }

    pub fn target_mac(&self) -> Option<MacAddr> {
        if self.hlen == 6 {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(self.target_hw_addr());
            Some(MacAddr(mac))
        } else {
            None
        }
    }

    pub fn sender_ip(&self) -> Option<Ipv4Addr> {
        if self.plen == 4 {
            let b = self.sender_proto_addr();
            Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
        } else {
            None
        }
    }

    pub fn target_ip(&self) -> Option<Ipv4Addr> {
        if self.plen == 4 {
            let b = self.target_proto_addr();
            Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
        } else {
            None
        }
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        let total = 8 + 2 * self.hlen + 2 * self.plen;
        &self.data[..total]
    }
}

pub struct ArpPacketBuilder {
    pub opcode: u16,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl ArpPacketBuilder {
    pub const LEN: usize = 28;

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        ensure_out_buf_len(out, Self::LEN)?;
        out[0..2].copy_from_slice(&HTYPE_ETHERNET.to_be_bytes());
        out[2..4].copy_from_slice(&PTYPE_IPV4.to_be_bytes());
        out[4] = 6;
        out[5] = 4;
        out[6..8].copy_from_slice(&self.opcode.to_be_bytes());
        out[8..14].copy_from_slice(&self.sender_mac.0);
        out[14..18].copy_from_slice(&self.sender_ip.octets());
        out[18..24].copy_from_slice(&self.target_mac.0);
        out[24..28].copy_from_slice(&self.target_ip.octets());
        Ok(Self::LEN)
    }
}

pub struct ArpReplyFrameBuilder {
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl ArpReplyFrameBuilder {
    pub fn len(&self) -> usize {
        super::ethernet::EthernetFrame::HEADER_LEN + ArpPacketBuilder::LEN
    }

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        let mut arp_buf = [0u8; ArpPacketBuilder::LEN];
        ArpPacketBuilder {
            opcode: ARP_OP_REPLY,
            sender_mac: self.sender_mac,
            sender_ip: self.sender_ip,
            target_mac: self.target_mac,
            target_ip: self.target_ip,
        }
        .write(&mut arp_buf)?;

        super::ethernet::EthernetFrameBuilder {
            dest_mac: self.target_mac,
            src_mac: self.sender_mac,
            ethertype: super::ethernet::ETHERTYPE_ARP,
            payload: &arp_buf,
        }
        .write(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ethernet_ipv4_arp_reply() {
        let builder = ArpPacketBuilder {
            opcode: ARP_OP_REPLY,
            sender_mac: MacAddr([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]),
            sender_ip: Ipv4Addr::new(10, 0, 0, 1),
            target_mac: MacAddr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            target_ip: Ipv4Addr::new(10, 0, 0, 2),
        };
        let mut buf = [0u8; 64];
        let len = builder.write(&mut buf).unwrap();
        let pkt = ArpPacket::parse(&buf[..len]).unwrap();
        assert_eq!(pkt.htype(), HTYPE_ETHERNET);
        assert_eq!(pkt.ptype(), PTYPE_IPV4);
        assert_eq!(pkt.opcode(), ARP_OP_REPLY);
        assert_eq!(pkt.sender_mac().unwrap().0, builder.sender_mac.0);
        assert_eq!(pkt.sender_ip().unwrap(), builder.sender_ip);
        assert_eq!(pkt.target_mac().unwrap().0, builder.target_mac.0);
        assert_eq!(pkt.target_ip().unwrap(), builder.target_ip);
    }
}

