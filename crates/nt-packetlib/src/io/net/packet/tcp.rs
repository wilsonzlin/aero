use core::net::Ipv4Addr;

use super::{checksum, ensure_len, ensure_out_buf_len, PacketError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpFlags(pub u16);

impl TcpFlags {
    pub const FIN: TcpFlags = TcpFlags(0x0001);
    pub const SYN: TcpFlags = TcpFlags(0x0002);
    pub const RST: TcpFlags = TcpFlags(0x0004);
    pub const PSH: TcpFlags = TcpFlags(0x0008);
    pub const ACK: TcpFlags = TcpFlags(0x0010);
    pub const URG: TcpFlags = TcpFlags(0x0020);
    pub const ECE: TcpFlags = TcpFlags(0x0040);
    pub const CWR: TcpFlags = TcpFlags(0x0080);
    pub const NS: TcpFlags = TcpFlags(0x0100);

    pub fn contains(self, other: TcpFlags) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for TcpFlags {
    type Output = TcpFlags;

    fn bitor(self, rhs: TcpFlags) -> Self::Output {
        TcpFlags(self.0 | rhs.0)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TcpSegment<'a> {
    data: &'a [u8],
    header_len: usize,
}

impl<'a> TcpSegment<'a> {
    pub const MIN_HEADER_LEN: usize = 20;

    pub fn parse(data: &'a [u8]) -> Result<Self, PacketError> {
        ensure_len(data, Self::MIN_HEADER_LEN)?;
        let data_offset = data[12] >> 4;
        if data_offset < 5 {
            return Err(PacketError::Malformed("TCP data offset < 5"));
        }
        let header_len = (data_offset as usize) * 4;
        ensure_len(data, header_len)?;
        Ok(Self { data, header_len })
    }

    pub fn src_port(&self) -> u16 {
        u16::from_be_bytes([self.data[0], self.data[1]])
    }

    pub fn dst_port(&self) -> u16 {
        u16::from_be_bytes([self.data[2], self.data[3]])
    }

    pub fn seq_number(&self) -> u32 {
        u32::from_be_bytes([self.data[4], self.data[5], self.data[6], self.data[7]])
    }

    pub fn ack_number(&self) -> u32 {
        u32::from_be_bytes([self.data[8], self.data[9], self.data[10], self.data[11]])
    }

    pub fn flags(&self) -> TcpFlags {
        let ns = (self.data[12] & 0x01) as u16;
        let flags = self.data[13] as u16;
        TcpFlags((ns << 8) | flags)
    }

    pub fn window_size(&self) -> u16 {
        u16::from_be_bytes([self.data[14], self.data[15]])
    }

    pub fn checksum(&self) -> u16 {
        u16::from_be_bytes([self.data[16], self.data[17]])
    }

    pub fn urgent_pointer(&self) -> u16 {
        u16::from_be_bytes([self.data[18], self.data[19]])
    }

    pub fn options(&self) -> &'a [u8] {
        &self.data[Self::MIN_HEADER_LEN..self.header_len]
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.data[self.header_len..]
    }

    pub fn header_len(&self) -> usize {
        self.header_len
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.data
    }

    pub fn checksum_valid_ipv4(&self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr) -> bool {
        checksum::transport_checksum_ipv4(src_ip, dst_ip, 6, self.as_bytes()) == 0
    }
}

pub struct TcpSegmentBuilder<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq_number: u32,
    pub ack_number: u32,
    pub flags: TcpFlags,
    pub window_size: u16,
    pub urgent_pointer: u16,
    pub options: &'a [u8],
    pub payload: &'a [u8],
}

impl<'a> TcpSegmentBuilder<'a> {
    pub fn syn_ack(
        src_port: u16,
        dst_port: u16,
        seq_number: u32,
        ack_number: u32,
        window_size: u16,
    ) -> Self {
        Self {
            src_port,
            dst_port,
            seq_number,
            ack_number,
            flags: TcpFlags::SYN | TcpFlags::ACK,
            window_size,
            urgent_pointer: 0,
            options: &[],
            payload: &[],
        }
    }

    pub fn ack(src_port: u16, dst_port: u16, seq_number: u32, ack_number: u32, window_size: u16) -> Self {
        Self {
            src_port,
            dst_port,
            seq_number,
            ack_number,
            flags: TcpFlags::ACK,
            window_size,
            urgent_pointer: 0,
            options: &[],
            payload: &[],
        }
    }

    pub fn rst(src_port: u16, dst_port: u16, seq_number: u32, ack_number: u32, window_size: u16) -> Self {
        Self {
            src_port,
            dst_port,
            seq_number,
            ack_number,
            flags: TcpFlags::RST | TcpFlags::ACK,
            window_size,
            urgent_pointer: 0,
            options: &[],
            payload: &[],
        }
    }

    pub fn fin_ack(
        src_port: u16,
        dst_port: u16,
        seq_number: u32,
        ack_number: u32,
        window_size: u16,
    ) -> Self {
        Self {
            src_port,
            dst_port,
            seq_number,
            ack_number,
            flags: TcpFlags::FIN | TcpFlags::ACK,
            window_size,
            urgent_pointer: 0,
            options: &[],
            payload: &[],
        }
    }

    pub fn header_len(&self) -> Result<usize, PacketError> {
        if !self.options.is_empty() && self.options.len() % 4 != 0 {
            return Err(PacketError::Malformed("TCP options length not multiple of 4"));
        }
        let header_len = TcpSegment::MIN_HEADER_LEN + self.options.len();
        if header_len / 4 > 0x0f {
            return Err(PacketError::Malformed("TCP header too large"));
        }
        Ok(header_len)
    }

    pub fn len(&self) -> Result<usize, PacketError> {
        Ok(self.header_len()? + self.payload.len())
    }

    #[cfg(feature = "alloc")]
    pub fn build_vec(&self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr) -> Result<alloc::vec::Vec<u8>, PacketError> {
        let len = self.len()?;
        let mut buf = alloc::vec![0u8; len];
        let written = self.write(src_ip, dst_ip, &mut buf)?;
        debug_assert_eq!(written, buf.len());
        Ok(buf)
    }

    pub fn write(&self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr, out: &mut [u8]) -> Result<usize, PacketError> {
        let header_len = self.header_len()?;
        let len = self.len()?;
        ensure_out_buf_len(out, len)?;

        out[0..2].copy_from_slice(&self.src_port.to_be_bytes());
        out[2..4].copy_from_slice(&self.dst_port.to_be_bytes());
        out[4..8].copy_from_slice(&self.seq_number.to_be_bytes());
        out[8..12].copy_from_slice(&self.ack_number.to_be_bytes());

        let data_offset = (header_len / 4) as u8;
        let ns = if self.flags.contains(TcpFlags::NS) { 1u8 } else { 0u8 };
        out[12] = (data_offset << 4) | ns;
        out[13] = (self.flags.0 & 0xff) as u8;

        out[14..16].copy_from_slice(&self.window_size.to_be_bytes());
        out[16..18].copy_from_slice(&0u16.to_be_bytes());
        out[18..20].copy_from_slice(&self.urgent_pointer.to_be_bytes());
        out[20..header_len].copy_from_slice(self.options);
        out[header_len..len].copy_from_slice(self.payload);

        // Unlike UDP, TCP has no "checksum disabled" sentinel value; a computed checksum
        // of 0x0000 is valid and must be written as-is.
        let csum = checksum::transport_checksum_ipv4(src_ip, dst_ip, 6, &out[..len]);
        out[16..18].copy_from_slice(&csum.to_be_bytes());
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_syn_ack() {
        let src_ip = Ipv4Addr::new(192, 0, 2, 1);
        let dst_ip = Ipv4Addr::new(192, 0, 2, 2);
        let builder = TcpSegmentBuilder::syn_ack(1000, 80, 1, 0, 1024);
        let mut buf = [0u8; 64];
        let len = builder.write(src_ip, dst_ip, &mut buf).unwrap();
        let seg = TcpSegment::parse(&buf[..len]).unwrap();
        assert_eq!(seg.src_port(), 1000);
        assert_eq!(seg.dst_port(), 80);
        assert_eq!(seg.seq_number(), 1);
        assert_eq!(seg.flags(), TcpFlags::SYN | TcpFlags::ACK);
        assert!(seg.checksum_valid_ipv4(src_ip, dst_ip));
    }

    #[test]
    fn tcp_checksum_can_be_zero() {
        let src_ip = Ipv4Addr::new(10, 0, 0, 1);
        let dst_ip = Ipv4Addr::new(10, 0, 0, 2);

        // Construct a minimal TCP segment whose correct checksum is exactly 0x0000.
        // (This is valid for TCP; only UDP treats 0x0000 as a special "checksum disabled"
        // marker.)
        let mut base = [0u8; 22];
        base[0..2].copy_from_slice(&1u16.to_be_bytes());
        base[2..4].copy_from_slice(&2u16.to_be_bytes());
        base[4..8].copy_from_slice(&1u32.to_be_bytes());
        base[8..12].copy_from_slice(&0u32.to_be_bytes());
        base[12] = 0x50; // data offset = 5
        base[13] = TcpFlags::ACK.0 as u8;
        base[14..16].copy_from_slice(&1024u16.to_be_bytes());
        base[16..18].copy_from_slice(&0u16.to_be_bytes());
        base[18..20].copy_from_slice(&0u16.to_be_bytes());
        base[20..22].copy_from_slice(&0u16.to_be_bytes());

        let sum_folded = !checksum::transport_checksum_ipv4(src_ip, dst_ip, 6, &base);
        let payload_word = 0xffffu16.wrapping_sub(sum_folded);
        let payload = payload_word.to_be_bytes();

        let builder = TcpSegmentBuilder {
            src_port: 1,
            dst_port: 2,
            seq_number: 1,
            ack_number: 0,
            flags: TcpFlags::ACK,
            window_size: 1024,
            urgent_pointer: 0,
            options: &[],
            payload: &payload,
        };
        let mut buf = [0u8; 64];
        let len = builder.write(src_ip, dst_ip, &mut buf).unwrap();
        let seg = TcpSegment::parse(&buf[..len]).unwrap();

        assert_eq!(seg.checksum(), 0);
        assert!(seg.checksum_valid_ipv4(src_ip, dst_ip));
        assert_eq!(checksum::transport_checksum_ipv4(src_ip, dst_ip, 6, &buf[..len]), 0);
    }
}
