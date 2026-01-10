use super::{ensure_len, ensure_out_buf_len, MacAddr, PacketError};

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;

/// An Ethernet II frame.
#[derive(Clone, Copy, Debug)]
pub struct EthernetFrame<'a> {
    data: &'a [u8],
}

impl<'a> EthernetFrame<'a> {
    pub const HEADER_LEN: usize = 14;

    pub fn parse(data: &'a [u8]) -> Result<Self, PacketError> {
        ensure_len(data, Self::HEADER_LEN)?;
        Ok(Self { data })
    }

    pub fn dest_mac(&self) -> MacAddr {
        let mut b = [0u8; 6];
        b.copy_from_slice(&self.data[0..6]);
        MacAddr(b)
    }

    pub fn src_mac(&self) -> MacAddr {
        let mut b = [0u8; 6];
        b.copy_from_slice(&self.data[6..12]);
        MacAddr(b)
    }

    pub fn ethertype(&self) -> u16 {
        u16::from_be_bytes([self.data[12], self.data[13]])
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.data[Self::HEADER_LEN..]
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.data
    }
}

/// Serialize an Ethernet II frame into an output buffer.
pub struct EthernetFrameBuilder<'a> {
    pub dest_mac: MacAddr,
    pub src_mac: MacAddr,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

impl<'a> EthernetFrameBuilder<'a> {
    pub fn len(&self) -> usize {
        EthernetFrame::HEADER_LEN + self.payload.len()
    }

    #[cfg(feature = "alloc")]
    pub fn build_vec(&self) -> Result<alloc::vec::Vec<u8>, PacketError> {
        let mut buf = alloc::vec![0u8; self.len()];
        let len = self.write(&mut buf)?;
        debug_assert_eq!(len, buf.len());
        Ok(buf)
    }

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        let needed = self.len();
        ensure_out_buf_len(out, needed)?;
        out[0..6].copy_from_slice(&self.dest_mac.0);
        out[6..12].copy_from_slice(&self.src_mac.0);
        out[12..14].copy_from_slice(&self.ethertype.to_be_bytes());
        out[14..needed].copy_from_slice(self.payload);
        Ok(needed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_parse_build() {
        let payload = [1u8, 2, 3, 4];
        let builder = EthernetFrameBuilder {
            dest_mac: MacAddr([0, 1, 2, 3, 4, 5]),
            src_mac: MacAddr([6, 7, 8, 9, 10, 11]),
            ethertype: ETHERTYPE_IPV4,
            payload: &payload,
        };
        let mut buf = [0u8; 64];
        let len = builder.write(&mut buf).unwrap();
        let frame = EthernetFrame::parse(&buf[..len]).unwrap();
        assert_eq!(frame.dest_mac().0, [0, 1, 2, 3, 4, 5]);
        assert_eq!(frame.src_mac().0, [6, 7, 8, 9, 10, 11]);
        assert_eq!(frame.ethertype(), ETHERTYPE_IPV4);
        assert_eq!(frame.payload(), &payload);
    }
}
