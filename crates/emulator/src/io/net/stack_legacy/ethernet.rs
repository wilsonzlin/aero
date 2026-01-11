use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr([u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xff; 6]);

    pub const fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    pub fn octets(self) -> [u8; 6] {
        self.0
    }

    pub fn is_broadcast(self) -> bool {
        self.0 == [0xff; 6]
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EtherType {
    Ipv4,
    Arp,
    Other(u16),
}

impl EtherType {
    pub fn from_u16(v: u16) -> Self {
        match v {
            0x0800 => Self::Ipv4,
            0x0806 => Self::Arp,
            other => Self::Other(other),
        }
    }

    pub fn to_u16(self) -> u16 {
        match self {
            EtherType::Ipv4 => 0x0800,
            EtherType::Arp => 0x0806,
            EtherType::Other(v) => v,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EthernetFrame<'a> {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: EtherType,
    pub payload: &'a [u8],
}

impl<'a> EthernetFrame<'a> {
    pub fn parse(frame: &'a [u8]) -> Option<Self> {
        if frame.len() < 14 {
            return None;
        }
        let dst = MacAddr::new(frame[0..6].try_into().ok()?);
        let src = MacAddr::new(frame[6..12].try_into().ok()?);
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        Some(Self {
            dst,
            src,
            ethertype: EtherType::from_u16(ethertype),
            payload: &frame[14..],
        })
    }
}

pub fn build_ethernet_frame(dst: MacAddr, src: MacAddr, ethertype: EtherType, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + payload.len());
    out.extend_from_slice(&dst.octets());
    out.extend_from_slice(&src.octets());
    out.extend_from_slice(&ethertype.to_u16().to_be_bytes());
    out.extend_from_slice(payload);
    out
}

