use std::net::Ipv4Addr;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpProtocol {
    Icmp = 1,
    Tcp = 6,
    Udp = 17,
    Other(u8),
}

impl IpProtocol {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Icmp,
            6 => Self::Tcp,
            17 => Self::Udp,
            other => Self::Other(other),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::Icmp => 1,
            Self::Tcp => 6,
            Self::Udp => 17,
            Self::Other(v) => v,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ipv4Packet<'a> {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub protocol: IpProtocol,
    pub ttl: u8,
    pub identification: u16,
    pub flags_fragment: u16,
    pub header_len: usize,
    pub total_len: usize,
    pub payload: &'a [u8],
}

impl<'a> Ipv4Packet<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < 20 {
            return None;
        }
        let version = buf[0] >> 4;
        if version != 4 {
            return None;
        }
        let ihl_words = (buf[0] & 0x0f) as usize;
        if ihl_words < 5 {
            return None;
        }
        let header_len = ihl_words * 4;
        if buf.len() < header_len {
            return None;
        }
        let total_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        if total_len < header_len || total_len > buf.len() {
            return None;
        }

        // Validate checksum (optional; drop if it doesn't match to avoid confusing later layers).
        if checksum(&buf[..header_len]) != 0 {
            return None;
        }

        let identification = u16::from_be_bytes([buf[4], buf[5]]);
        let flags_fragment = u16::from_be_bytes([buf[6], buf[7]]);
        let ttl = buf[8];
        let protocol = IpProtocol::from_u8(buf[9]);
        let src = Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]);
        let dst = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);
        Some(Self {
            src,
            dst,
            protocol,
            ttl,
            identification,
            flags_fragment,
            header_len,
            total_len,
            payload: &buf[header_len..total_len],
        })
    }

    pub fn is_fragmented(&self) -> bool {
        let more_fragments = (self.flags_fragment & 0x2000) != 0;
        let fragment_offset = self.flags_fragment & 0x1fff;
        more_fragments || fragment_offset != 0
    }
}

pub struct Ipv4PacketBuilder {
    src: Ipv4Addr,
    dst: Ipv4Addr,
    protocol: IpProtocol,
    ttl: u8,
    identification: u16,
    flags_fragment: u16,
    payload: Vec<u8>,
}

impl Ipv4PacketBuilder {
    pub fn new() -> Self {
        Self {
            src: Ipv4Addr::UNSPECIFIED,
            dst: Ipv4Addr::UNSPECIFIED,
            protocol: IpProtocol::Other(0),
            ttl: 64,
            identification: 0,
            flags_fragment: 0x4000, // Don't Fragment
            payload: Vec::new(),
        }
    }

    pub fn src(mut self, ip: Ipv4Addr) -> Self {
        self.src = ip;
        self
    }

    pub fn dst(mut self, ip: Ipv4Addr) -> Self {
        self.dst = ip;
        self
    }

    pub fn protocol(mut self, proto: IpProtocol) -> Self {
        self.protocol = proto;
        self
    }

    pub fn ttl(mut self, ttl: u8) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn identification(mut self, id: u16) -> Self {
        self.identification = id;
        self
    }

    pub fn flags_fragment(mut self, ff: u16) -> Self {
        self.flags_fragment = ff;
        self
    }

    pub fn payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    pub fn build(self) -> Vec<u8> {
        let header_len = 20usize;
        let total_len = header_len + self.payload.len();
        let mut out = Vec::with_capacity(total_len);

        out.push((4u8 << 4) | 5u8); // version + IHL
        out.push(0); // DSCP/ECN
        out.extend_from_slice(&(total_len as u16).to_be_bytes());
        out.extend_from_slice(&self.identification.to_be_bytes());
        out.extend_from_slice(&self.flags_fragment.to_be_bytes());
        out.push(self.ttl);
        out.push(self.protocol.to_u8());
        out.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
        out.extend_from_slice(&self.src.octets());
        out.extend_from_slice(&self.dst.octets());

        let csum = checksum(&out);
        out[10..12].copy_from_slice(&csum.to_be_bytes());

        out.extend_from_slice(&self.payload);
        out
    }
}

/// Compute the IPv4-style checksum (one's complement sum).
///
/// For validation, call this on the full header including the checksum field; a valid header will
/// yield `0`.
pub fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&rem) = chunks.remainder().get(0) {
        sum += (rem as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn checksum_pseudo_header(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, len: u16) -> u32 {
    let mut sum: u32 = 0;
    sum += u16::from_be_bytes([src.octets()[0], src.octets()[1]]) as u32;
    sum += u16::from_be_bytes([src.octets()[2], src.octets()[3]]) as u32;
    sum += u16::from_be_bytes([dst.octets()[0], dst.octets()[1]]) as u32;
    sum += u16::from_be_bytes([dst.octets()[2], dst.octets()[3]]) as u32;
    sum += protocol as u32;
    sum += len as u32;
    sum
}

pub fn finalize_checksum(mut sum: u32) -> u16 {
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[derive(Debug, Clone, Copy)]
pub struct IcmpPacket<'a> {
    pub typ: u8,
    pub code: u8,
    pub checksum: u16,
    pub rest: [u8; 4],
    pub payload: &'a [u8],
}

impl<'a> IcmpPacket<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < 8 {
            return None;
        }
        if checksum(buf) != 0 {
            return None;
        }
        Some(Self {
            typ: buf[0],
            code: buf[1],
            checksum: u16::from_be_bytes([buf[2], buf[3]]),
            rest: buf[4..8].try_into().ok()?,
            payload: &buf[8..],
        })
    }

    pub fn is_echo_request(&self) -> bool {
        self.typ == 8 && self.code == 0
    }

    pub fn build_echo_reply(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.payload.len());
        out.push(0); // Echo reply
        out.push(self.code);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&self.rest);
        out.extend_from_slice(self.payload);
        let csum = checksum(&out);
        out[2..4].copy_from_slice(&csum.to_be_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_checksum_round_trip() {
        let pkt = Ipv4PacketBuilder::new()
            .src(Ipv4Addr::new(192, 0, 2, 1))
            .dst(Ipv4Addr::new(198, 51, 100, 2))
            .protocol(IpProtocol::Udp)
            .ttl(64)
            .identification(0x1234)
            .payload(vec![1, 2, 3, 4, 5, 6, 7, 8])
            .build();

        assert!(Ipv4Packet::parse(&pkt).is_some(), "packet should parse");

        // Corrupt a header byte and ensure checksum validation fails.
        let mut corrupted = pkt.clone();
        corrupted[1] ^= 0xff;
        assert!(Ipv4Packet::parse(&corrupted).is_none(), "corrupted checksum should fail");
    }
}
