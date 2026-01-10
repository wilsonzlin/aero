use super::{checksum, ensure_len, ensure_out_buf_len, PacketError};

#[derive(Clone, Copy, Debug)]
pub struct Icmpv4Packet<'a> {
    data: &'a [u8],
}

impl<'a> Icmpv4Packet<'a> {
    pub const MIN_LEN: usize = 4;

    pub fn parse(data: &'a [u8]) -> Result<Self, PacketError> {
        ensure_len(data, Self::MIN_LEN)?;
        Ok(Self { data })
    }

    pub fn icmp_type(&self) -> u8 {
        self.data[0]
    }

    pub fn code(&self) -> u8 {
        self.data[1]
    }

    pub fn checksum(&self) -> u16 {
        u16::from_be_bytes([self.data[2], self.data[3]])
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.data[4..]
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.data
    }

    pub fn checksum_valid(&self) -> bool {
        checksum::internet_checksum(self.as_bytes()) == 0
    }

    pub fn echo(&self) -> Option<IcmpEcho<'a>> {
        match self.icmp_type() {
            0 | 8 => {
                if self.code() != 0 || self.data.len() < 8 {
                    return None;
                }
                let id = u16::from_be_bytes([self.data[4], self.data[5]]);
                let seq = u16::from_be_bytes([self.data[6], self.data[7]]);
                Some(IcmpEcho {
                    icmp_type: self.icmp_type(),
                    identifier: id,
                    sequence: seq,
                    payload: &self.data[8..],
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcmpEcho<'a> {
    pub icmp_type: u8,
    pub identifier: u16,
    pub sequence: u16,
    pub payload: &'a [u8],
}

pub struct IcmpEchoBuilder<'a> {
    pub icmp_type: u8,
    pub identifier: u16,
    pub sequence: u16,
    pub payload: &'a [u8],
}

impl<'a> IcmpEchoBuilder<'a> {
    pub fn echo_request(identifier: u16, sequence: u16, payload: &'a [u8]) -> Self {
        Self {
            icmp_type: 8,
            identifier,
            sequence,
            payload,
        }
    }

    pub fn echo_reply(identifier: u16, sequence: u16, payload: &'a [u8]) -> Self {
        Self {
            icmp_type: 0,
            identifier,
            sequence,
            payload,
        }
    }

    pub fn len(&self) -> usize {
        8 + self.payload.len()
    }

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        let len = self.len();
        ensure_out_buf_len(out, len)?;
        out[0] = self.icmp_type;
        out[1] = 0;
        out[2..4].copy_from_slice(&0u16.to_be_bytes());
        out[4..6].copy_from_slice(&self.identifier.to_be_bytes());
        out[6..8].copy_from_slice(&self.sequence.to_be_bytes());
        out[8..len].copy_from_slice(self.payload);
        let csum = checksum::internet_checksum(&out[..len]);
        out[2..4].copy_from_slice(&csum.to_be_bytes());
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_echo_reply() {
        let payload = *b"ping";
        let builder = IcmpEchoBuilder::echo_reply(0x1234, 0x0001, &payload);
        let mut buf = [0u8; 64];
        let len = builder.write(&mut buf).unwrap();
        let pkt = Icmpv4Packet::parse(&buf[..len]).unwrap();
        assert!(pkt.checksum_valid());
        let echo = pkt.echo().unwrap();
        assert_eq!(echo.icmp_type, 0);
        assert_eq!(echo.identifier, 0x1234);
        assert_eq!(echo.sequence, 0x0001);
        assert_eq!(echo.payload, &payload);
    }

    #[test]
    fn build_and_parse_echo_request() {
        let payload = *b"ping";
        let builder = IcmpEchoBuilder::echo_request(0x1234, 0x0001, &payload);
        let mut buf = [0u8; 64];
        let len = builder.write(&mut buf).unwrap();
        let pkt = Icmpv4Packet::parse(&buf[..len]).unwrap();
        assert!(pkt.checksum_valid());
        let echo = pkt.echo().unwrap();
        assert_eq!(echo.icmp_type, 8);
        assert_eq!(echo.identifier, 0x1234);
        assert_eq!(echo.sequence, 0x0001);
        assert_eq!(echo.payload, &payload);
    }
}
