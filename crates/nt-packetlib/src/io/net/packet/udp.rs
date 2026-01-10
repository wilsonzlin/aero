use core::net::Ipv4Addr;

use super::{checksum, ensure_len, ensure_out_buf_len, PacketError};

#[derive(Clone, Copy, Debug)]
pub struct UdpPacket<'a> {
    data: &'a [u8],
    length: usize,
}

impl<'a> UdpPacket<'a> {
    pub const HEADER_LEN: usize = 8;

    pub fn parse(data: &'a [u8]) -> Result<Self, PacketError> {
        ensure_len(data, Self::HEADER_LEN)?;
        let length = u16::from_be_bytes([data[4], data[5]]) as usize;
        if length < Self::HEADER_LEN {
            return Err(PacketError::Malformed("UDP length < header length"));
        }
        ensure_len(data, length)?;
        Ok(Self { data, length })
    }

    pub fn src_port(&self) -> u16 {
        u16::from_be_bytes([self.data[0], self.data[1]])
    }

    pub fn dst_port(&self) -> u16 {
        u16::from_be_bytes([self.data[2], self.data[3]])
    }

    pub fn length(&self) -> u16 {
        self.length as u16
    }

    pub fn checksum(&self) -> u16 {
        u16::from_be_bytes([self.data[6], self.data[7]])
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.data[Self::HEADER_LEN..self.length]
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        &self.data[..self.length]
    }

    pub fn checksum_valid_ipv4(&self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr) -> bool {
        let csum = self.checksum();
        if csum == 0 {
            return true;
        }
        checksum::transport_checksum_ipv4(src_ip, dst_ip, 17, self.as_bytes()) == 0
    }
}

pub struct UdpPacketBuilder<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: &'a [u8],
}

impl<'a> UdpPacketBuilder<'a> {
    pub fn len(&self) -> Result<usize, PacketError> {
        let len = UdpPacket::HEADER_LEN + self.payload.len();
        if len > u16::MAX as usize {
            return Err(PacketError::Malformed("UDP length > 65535"));
        }
        Ok(len)
    }

    pub fn write(&self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr, out: &mut [u8]) -> Result<usize, PacketError> {
        let len = self.len()?;
        ensure_out_buf_len(out, len)?;
        out[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        out[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        out[4..6].copy_from_slice(&(len as u16).to_be_bytes());
        out[6..8].copy_from_slice(&0u16.to_be_bytes());
        out[8..len].copy_from_slice(self.payload);
        let mut csum = checksum::transport_checksum_ipv4(src_ip, dst_ip, 17, &out[..len]);
        if csum == 0 {
            csum = 0xffff;
        }
        out[6..8].copy_from_slice(&csum.to_be_bytes());
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_udp() {
        let payload = *b"hello";
        let src_ip = Ipv4Addr::new(10, 0, 0, 1);
        let dst_ip = Ipv4Addr::new(10, 0, 0, 2);
        let builder = UdpPacketBuilder {
            src_port: 1234,
            dst_port: 53,
            payload: &payload,
        };
        let mut buf = [0u8; 64];
        let len = builder.write(src_ip, dst_ip, &mut buf).unwrap();
        let pkt = UdpPacket::parse(&buf[..len]).unwrap();
        assert_eq!(pkt.src_port(), 1234);
        assert_eq!(pkt.dst_port(), 53);
        assert_eq!(pkt.payload(), &payload);
        assert!(pkt.checksum_valid_ipv4(src_ip, dst_ip));
    }

    #[test]
    fn udp_checksum_zero_is_transmitted_as_ffff() {
        let src_ip = Ipv4Addr::new(10, 0, 0, 1);
        let dst_ip = Ipv4Addr::new(10, 0, 0, 2);

        // Craft a 2-byte payload that makes the computed checksum exactly 0x0000,
        // then ensure we transmit 0xffff (RFC 768: checksum optional sentinel).
        let mut base = [0u8; 10];
        base[0..2].copy_from_slice(&1u16.to_be_bytes());
        base[2..4].copy_from_slice(&2u16.to_be_bytes());
        base[4..6].copy_from_slice(&(10u16).to_be_bytes());
        base[6..8].copy_from_slice(&0u16.to_be_bytes());
        base[8..10].copy_from_slice(&0u16.to_be_bytes());

        let sum_folded = !checksum::transport_checksum_ipv4(src_ip, dst_ip, 17, &base);
        let payload_word = 0xffffu16.wrapping_sub(sum_folded);
        let payload = payload_word.to_be_bytes();

        let builder = UdpPacketBuilder {
            src_port: 1,
            dst_port: 2,
            payload: &payload,
        };
        let mut buf = [0u8; 64];
        let len = builder.write(src_ip, dst_ip, &mut buf).unwrap();
        let pkt = UdpPacket::parse(&buf[..len]).unwrap();

        assert_eq!(pkt.checksum(), 0xffff);
        assert!(pkt.checksum_valid_ipv4(src_ip, dst_ip));
        assert_eq!(checksum::transport_checksum_ipv4(src_ip, dst_ip, 17, pkt.as_bytes()), 0);
    }
}
