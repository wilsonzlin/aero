//! Minimal DNS response builder (A record).
//!
//! DNS lives above UDP; the network stack may still want to synthesize
//! responses for convenience (e.g. local hostnames).

use core::net::Ipv4Addr;

use super::{ensure_out_buf_len, PacketError};

/// A single-question DNS query (as used in basic A record lookups).
#[derive(Clone, Copy, Debug)]
pub struct DnsQuery<'a> {
    pub id: u16,
    pub qname: &'a [u8],
    pub qtype: u16,
    pub qclass: u16,
}

/// Parse a DNS query containing exactly one question.
pub fn parse_single_query(packet: &[u8]) -> Result<DnsQuery<'_>, PacketError> {
    super::ensure_len(packet, 12)?;
    let id = u16::from_be_bytes([packet[0], packet[1]]);
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    if qdcount != 1 {
        return Err(PacketError::Unsupported("DNS queries with qdcount != 1"));
    }

    let mut off = 12;
    // QNAME is a series of length-prefixed labels terminated by 0.
    while off < packet.len() {
        let len = packet[off] as usize;
        off += 1;
        if len == 0 {
            break;
        }
        super::ensure_len(packet, off + len)?;
        off += len;
    }
    if off + 4 > packet.len() {
        return Err(PacketError::Truncated {
            needed: off + 4,
            actual: packet.len(),
        });
    }
    let qname = &packet[12..off];
    let qtype = u16::from_be_bytes([packet[off], packet[off + 1]]);
    let qclass = u16::from_be_bytes([packet[off + 2], packet[off + 3]]);
    Ok(DnsQuery {
        id,
        qname,
        qtype,
        qclass,
    })
}

pub struct DnsAResponseBuilder<'a> {
    pub id: u16,
    pub qname: &'a [u8],
    pub addr: Ipv4Addr,
    /// TTL in seconds.
    pub ttl: u32,
}

impl<'a> DnsAResponseBuilder<'a> {
    pub fn len(&self) -> usize {
        // Header (12) + question (qname + 4) + answer (name ptr 2 + type 2 + class 2 + ttl 4 + rdlen 2 + rdata 4)
        12 + self.qname.len() + 4 + 2 + 2 + 2 + 4 + 2 + 4
    }

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        let len = self.len();
        ensure_out_buf_len(out, len)?;

        // Header
        out[0..2].copy_from_slice(&self.id.to_be_bytes());
        out[2..4].copy_from_slice(&0x8180u16.to_be_bytes()); // standard response, recursion available, no error
        out[4..6].copy_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        out[6..8].copy_from_slice(&1u16.to_be_bytes()); // ANCOUNT
        out[8..10].copy_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        out[10..12].copy_from_slice(&0u16.to_be_bytes()); // ARCOUNT

        let mut off = 12;
        out[off..off + self.qname.len()].copy_from_slice(self.qname);
        off += self.qname.len();
        out[off..off + 2].copy_from_slice(&1u16.to_be_bytes()); // QTYPE=A
        out[off + 2..off + 4].copy_from_slice(&1u16.to_be_bytes()); // QCLASS=IN
        off += 4;

        // Answer name: pointer to offset 12 (0x0c)
        out[off..off + 2].copy_from_slice(&0xc00cu16.to_be_bytes());
        off += 2;
        out[off..off + 2].copy_from_slice(&1u16.to_be_bytes()); // TYPE=A
        off += 2;
        out[off..off + 2].copy_from_slice(&1u16.to_be_bytes()); // CLASS=IN
        off += 2;
        out[off..off + 4].copy_from_slice(&self.ttl.to_be_bytes());
        off += 4;
        out[off..off + 2].copy_from_slice(&4u16.to_be_bytes());
        off += 2;
        out[off..off + 4].copy_from_slice(&self.addr.octets());
        off += 4;

        debug_assert_eq!(off, len);
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_basic_query() {
        // Query for "a." (qname: 1,'a',0) type A class IN
        let query = [
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
            b'a', 0x00, 0x00, 0x01, 0x00, 0x01,
        ];
        let q = parse_single_query(&query).unwrap();
        assert_eq!(q.id, 0x1234);
        assert_eq!(q.qname, &[0x01, b'a', 0x00]);
        assert_eq!(q.qtype, 1);
        assert_eq!(q.qclass, 1);

        let builder = DnsAResponseBuilder {
            id: q.id,
            qname: q.qname,
            addr: Ipv4Addr::new(10, 0, 0, 1),
            ttl: 60,
        };
        let mut buf = [0u8; 128];
        let len = builder.write(&mut buf).unwrap();
        assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), 0x1234);
        assert_eq!(u16::from_be_bytes([buf[6], buf[7]]), 1); // ANCOUNT
        assert_eq!(buf[len - 4..len], [10, 0, 0, 1]);
    }
}

