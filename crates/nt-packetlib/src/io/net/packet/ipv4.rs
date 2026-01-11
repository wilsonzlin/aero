use core::net::Ipv4Addr;

use super::{checksum, ensure_len, ensure_out_buf_len, PacketError};

pub const IPPROTO_ICMP: u8 = 1;
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;

#[derive(Clone, Copy, Debug)]
pub struct Ipv4Packet<'a> {
    data: &'a [u8],
    header_len: usize,
    total_len: usize,
}

impl<'a> Ipv4Packet<'a> {
    pub const MIN_HEADER_LEN: usize = 20;

    pub fn parse(data: &'a [u8]) -> Result<Self, PacketError> {
        ensure_len(data, Self::MIN_HEADER_LEN)?;
        let v_ihl = data[0];
        let version = v_ihl >> 4;
        if version != 4 {
            return Err(PacketError::Malformed("IPv4 version != 4"));
        }
        let ihl = (v_ihl & 0x0f) as usize;
        if ihl < 5 {
            return Err(PacketError::Malformed("IPv4 IHL < 5"));
        }
        let header_len = ihl * 4;
        ensure_len(data, header_len)?;
        let total_len = u16::from_be_bytes([data[2], data[3]]) as usize;
        if total_len < header_len {
            return Err(PacketError::Malformed("IPv4 total length < header length"));
        }
        ensure_len(data, total_len)?;
        Ok(Self {
            data,
            header_len,
            total_len,
        })
    }

    pub fn header_len(&self) -> usize {
        self.header_len
    }

    pub fn total_len(&self) -> usize {
        self.total_len
    }

    pub fn dscp_ecn(&self) -> u8 {
        self.data[1]
    }

    pub fn identification(&self) -> u16 {
        u16::from_be_bytes([self.data[4], self.data[5]])
    }

    pub fn flags_fragment(&self) -> u16 {
        u16::from_be_bytes([self.data[6], self.data[7]])
    }

    pub fn ttl(&self) -> u8 {
        self.data[8]
    }

    pub fn protocol(&self) -> u8 {
        self.data[9]
    }

    pub fn header_checksum(&self) -> u16 {
        u16::from_be_bytes([self.data[10], self.data[11]])
    }

    pub fn src_ip(&self) -> Ipv4Addr {
        Ipv4Addr::new(self.data[12], self.data[13], self.data[14], self.data[15])
    }

    pub fn dst_ip(&self) -> Ipv4Addr {
        Ipv4Addr::new(self.data[16], self.data[17], self.data[18], self.data[19])
    }

    pub fn options(&self) -> &'a [u8] {
        &self.data[Self::MIN_HEADER_LEN..self.header_len]
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.data[self.header_len..self.total_len]
    }

    pub fn header_bytes(&self) -> &'a [u8] {
        &self.data[..self.header_len]
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        &self.data[..self.total_len]
    }

    pub fn checksum_valid(&self) -> bool {
        checksum::internet_checksum(self.header_bytes()) == 0
    }
}

pub struct Ipv4PacketBuilder<'a> {
    pub dscp_ecn: u8,
    pub identification: u16,
    pub flags_fragment: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub src_ip: Ipv4Addr,
    pub dst_ip: Ipv4Addr,
    pub options: &'a [u8],
    pub payload: &'a [u8],
}

impl<'a> Ipv4PacketBuilder<'a> {
    pub fn header_len(&self) -> Result<usize, PacketError> {
        if !self.options.is_empty() && self.options.len() % 4 != 0 {
            return Err(PacketError::Malformed(
                "IPv4 options length not multiple of 4",
            ));
        }
        let header_len = Ipv4Packet::MIN_HEADER_LEN + self.options.len();
        if header_len / 4 > 0x0f {
            return Err(PacketError::Malformed("IPv4 header too large"));
        }
        Ok(header_len)
    }

    pub fn total_len(&self) -> Result<usize, PacketError> {
        Ok(self.header_len()? + self.payload.len())
    }

    #[cfg(feature = "alloc")]
    pub fn build_vec(&self) -> Result<alloc::vec::Vec<u8>, PacketError> {
        let len = self.total_len()?;
        let mut buf = alloc::vec![0u8; len];
        let written = self.write(&mut buf)?;
        debug_assert_eq!(written, buf.len());
        Ok(buf)
    }

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        let header_len = self.header_len()?;
        let total_len = self.total_len()?;
        if total_len > u16::MAX as usize {
            return Err(PacketError::Malformed("IPv4 total length > 65535"));
        }
        ensure_out_buf_len(out, total_len)?;

        out[0] = (4u8 << 4) | ((header_len / 4) as u8);
        out[1] = self.dscp_ecn;
        out[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        out[4..6].copy_from_slice(&self.identification.to_be_bytes());
        out[6..8].copy_from_slice(&self.flags_fragment.to_be_bytes());
        out[8] = self.ttl;
        out[9] = self.protocol;
        out[10..12].copy_from_slice(&0u16.to_be_bytes());
        out[12..16].copy_from_slice(&self.src_ip.octets());
        out[16..20].copy_from_slice(&self.dst_ip.octets());
        out[20..header_len].copy_from_slice(self.options);
        out[header_len..total_len].copy_from_slice(self.payload);

        let csum = checksum::ipv4_header_checksum(&out[..header_len]);
        out[10..12].copy_from_slice(&csum.to_be_bytes());
        Ok(total_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_ipv4() {
        let payload = [1u8, 2, 3, 4];
        let builder = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: 0x1234,
            flags_fragment: 0x4000,
            ttl: 64,
            protocol: IPPROTO_UDP,
            src_ip: Ipv4Addr::new(10, 0, 0, 1),
            dst_ip: Ipv4Addr::new(10, 0, 0, 2),
            options: &[],
            payload: &payload,
        };
        let mut buf = [0u8; 64];
        let len = builder.write(&mut buf).unwrap();
        let pkt = Ipv4Packet::parse(&buf[..len]).unwrap();
        assert_eq!(pkt.src_ip(), builder.src_ip);
        assert_eq!(pkt.dst_ip(), builder.dst_ip);
        assert_eq!(pkt.protocol(), IPPROTO_UDP);
        assert_eq!(pkt.payload(), &payload);
        assert!(pkt.checksum_valid());
    }
}
