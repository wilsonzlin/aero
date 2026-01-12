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
    pub flags: u16,
    pub qname: &'a [u8],
    pub qtype: u16,
    pub qclass: u16,
}

impl<'a> DnsQuery<'a> {
    pub fn recursion_desired(&self) -> bool {
        (self.flags & 0x0100) != 0
    }

    #[cfg(feature = "alloc")]
    pub fn name(&self) -> Result<alloc::string::String, PacketError> {
        qname_to_string(self.qname)
    }
}

#[cfg(feature = "alloc")]
pub fn qname_to_string(qname: &[u8]) -> Result<alloc::string::String, PacketError> {
    use alloc::string::String;

    // RFC1035: domain names are limited to 255 bytes in wire format (including length octets and
    // the terminating 0-length label).
    if qname.len() > 255 {
        return Err(PacketError::Malformed("DNS QNAME too long"));
    }

    // Upper bound on the output length is `qname.len()`: the encoded name includes 1-byte label
    // length prefixes (and a 0 terminator), so the decoded string is always <= this.
    //
    // Pre-allocate to avoid repeated growth reallocations in hot paths (DNS policy checks, cache
    // keys, etc).
    let mut out = String::with_capacity(qname.len());
    let mut off = 0usize;
    while off < qname.len() {
        let len = qname[off] as usize;
        off += 1;
        if len == 0 {
            return Ok(out);
        }
        if len > 63 {
            return Err(PacketError::Malformed("DNS label length > 63"));
        }
        if off + len > qname.len() {
            return Err(PacketError::Truncated {
                needed: off + len,
                actual: qname.len(),
            });
        }
        let label = core::str::from_utf8(&qname[off..off + len])
            .map_err(|_| PacketError::Malformed("DNS label is not UTF-8"))?;
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(label);
        off += len;
    }

    Err(PacketError::Malformed("DNS QNAME missing terminator"))
}

fn parse_single_question_inner(packet: &[u8]) -> Result<DnsQuery<'_>, PacketError> {
    super::ensure_len(packet, 12)?;
    let id = u16::from_be_bytes([packet[0], packet[1]]);
    let flags = u16::from_be_bytes([packet[2], packet[3]]);
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]);
    if qdcount != 1 {
        return Err(PacketError::Unsupported("DNS queries with qdcount != 1"));
    }

    let mut off = 12;
    // QNAME is a series of length-prefixed labels terminated by 0.
    while off < packet.len() {
        let len_byte = packet[off];
        off += 1;
        // Name compression isn't expected for the simple queries we handle. Treat it as unsupported
        // so we don't accidentally interpret pointers as huge label lengths.
        if (len_byte & 0xc0) == 0xc0 {
            return Err(PacketError::Unsupported("compressed DNS QNAME"));
        }
        if (len_byte & 0xc0) != 0 {
            return Err(PacketError::Malformed(
                "DNS label length has reserved bits set",
            ));
        }
        let len = len_byte as usize;
        if len == 0 {
            break;
        }
        if len > 63 {
            return Err(PacketError::Malformed("DNS label length > 63"));
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
    // RFC1035: domain names are limited to 255 bytes in wire format.
    if qname.len() > 255 {
        return Err(PacketError::Malformed("DNS QNAME too long"));
    }
    let qtype = u16::from_be_bytes([packet[off], packet[off + 1]]);
    let qclass = u16::from_be_bytes([packet[off + 2], packet[off + 3]]);
    Ok(DnsQuery {
        id,
        flags,
        qname,
        qtype,
        qclass,
    })
}

/// Parse the question section of a DNS packet containing exactly one question.
///
/// Unlike [`parse_single_query`], this does not require the packet to have `QR=0` and is therefore
/// suitable for extracting the echoed question from DNS responses.
pub fn parse_single_question(packet: &[u8]) -> Result<DnsQuery<'_>, PacketError> {
    parse_single_question_inner(packet)
}

/// Parse a DNS query containing exactly one question.
pub fn parse_single_query(packet: &[u8]) -> Result<DnsQuery<'_>, PacketError> {
    let q = parse_single_question_inner(packet)?;
    // Must be a query (QR=0).
    if (q.flags & 0x8000) != 0 {
        return Err(PacketError::Malformed("DNS packet is not a query"));
    }
    Ok(q)
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsResponseCode {
    NoError = 0,
    FormatError = 1,
    ServerFailure = 2,
    NameError = 3,
    NotImplemented = 4,
    Refused = 5,
}

pub struct DnsResponseBuilder<'a> {
    pub id: u16,
    /// Recursion desired (echoed from the query).
    pub rd: bool,
    pub rcode: DnsResponseCode,
    pub qname: &'a [u8],
    pub qtype: u16,
    pub qclass: u16,
    /// If set, include a single A-record answer in the response.
    pub answer_a: Option<Ipv4Addr>,
    /// TTL in seconds for the A record.
    pub ttl: u32,
}

impl<'a> DnsResponseBuilder<'a> {
    pub fn len(&self) -> usize {
        let mut len = 12 + self.qname.len() + 4;
        if self.answer_a.is_some() {
            // Answer: name ptr 2 + type 2 + class 2 + ttl 4 + rdlen 2 + rdata 4
            len += 2 + 2 + 2 + 4 + 2 + 4;
        }
        len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        let len = self.len();
        ensure_out_buf_len(out, len)?;

        // Header
        out[0..2].copy_from_slice(&self.id.to_be_bytes());
        // Flags: standard response, recursion available, plus caller-specified RD/RCODE.
        let mut flags = 0x8000u16; // QR=1
        if self.rd {
            flags |= 0x0100;
        }
        flags |= 0x0080; // RA=1
        flags |= (self.rcode as u16) & 0x000f;
        out[2..4].copy_from_slice(&flags.to_be_bytes());
        out[4..6].copy_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        out[6..8].copy_from_slice(&(self.answer_a.is_some() as u16).to_be_bytes()); // ANCOUNT
        out[8..10].copy_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        out[10..12].copy_from_slice(&0u16.to_be_bytes()); // ARCOUNT

        let mut off = 12;
        out[off..off + self.qname.len()].copy_from_slice(self.qname);
        off += self.qname.len();
        out[off..off + 2].copy_from_slice(&self.qtype.to_be_bytes());
        out[off + 2..off + 4].copy_from_slice(&self.qclass.to_be_bytes());
        off += 4;

        if let Some(addr) = self.answer_a {
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
            out[off..off + 4].copy_from_slice(&addr.octets());
            off += 4;
        }

        debug_assert_eq!(off, len);
        Ok(len)
    }

    #[cfg(feature = "alloc")]
    pub fn build_vec(&self) -> Result<alloc::vec::Vec<u8>, PacketError> {
        let len = self.len();
        let mut buf = alloc::vec![0u8; len];
        let written = self.write(&mut buf)?;
        debug_assert_eq!(written, buf.len());
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_basic_query() {
        // Query for "a." (qname: 1,'a',0) type A class IN
        let query = [
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, b'a',
            0x00, 0x00, 0x01, 0x00, 0x01,
        ];
        let q = parse_single_query(&query).unwrap();
        assert_eq!(q.id, 0x1234);
        assert_eq!(q.flags, 0x0100);
        assert_eq!(q.qname, &[0x01, b'a', 0x00]);
        assert_eq!(q.qtype, 1);
        assert_eq!(q.qclass, 1);

        let builder = DnsResponseBuilder {
            id: q.id,
            rd: q.recursion_desired(),
            rcode: DnsResponseCode::NoError,
            qname: q.qname,
            qtype: q.qtype,
            qclass: q.qclass,
            answer_a: Some(Ipv4Addr::new(10, 0, 0, 1)),
            ttl: 60,
        };
        let mut buf = [0u8; 128];
        let len = builder.write(&mut buf).unwrap();
        assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), 0x1234);
        assert_eq!(u16::from_be_bytes([buf[6], buf[7]]), 1); // ANCOUNT
        assert_eq!(buf[len - 4..len], [10, 0, 0, 1]);
    }

    #[test]
    fn parse_single_question_accepts_responses() {
        // Query for "a." (qname: 1,'a',0) type A class IN
        let query = [
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, b'a',
            0x00, 0x00, 0x01, 0x00, 0x01,
        ];
        let q = parse_single_query(&query).unwrap();
        let builder = DnsResponseBuilder {
            id: q.id,
            rd: q.recursion_desired(),
            rcode: DnsResponseCode::NoError,
            qname: q.qname,
            qtype: q.qtype,
            qclass: q.qclass,
            answer_a: Some(Ipv4Addr::new(10, 0, 0, 1)),
            ttl: 60,
        };
        let resp = builder.build_vec().unwrap();

        let q_resp = parse_single_question(&resp).unwrap();
        assert_eq!(q_resp.id, q.id);
        assert_eq!(q_resp.qname, q.qname);
        assert_eq!(q_resp.qtype, q.qtype);
        assert_eq!(q_resp.qclass, q.qclass);
        assert_ne!(q_resp.flags & 0x8000, 0, "expected QR=1 in response");

        let err = parse_single_query(&resp).unwrap_err();
        assert_eq!(err, PacketError::Malformed("DNS packet is not a query"));
    }

    #[test]
    fn decode_qname_to_string() {
        let qname = [
            0x07u8, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00,
        ];
        assert_eq!(qname_to_string(&qname).unwrap(), "example.com");
    }

    #[test]
    fn build_nxdomain_response() {
        let qname = [0x01u8, b'a', 0x00];
        let builder = DnsResponseBuilder {
            id: 0x9999,
            rd: true,
            rcode: DnsResponseCode::NameError,
            qname: &qname,
            qtype: 1,
            qclass: 1,
            answer_a: None,
            ttl: 0,
        };
        let mut buf = [0u8; 128];
        let len = builder.write(&mut buf).unwrap();
        assert!(len >= 12 + qname.len() + 4);
        let flags = u16::from_be_bytes([buf[2], buf[3]]);
        assert_eq!(flags & 0x8000, 0x8000); // QR=1
        assert_eq!(flags & 0x0100, 0x0100); // RD echoed
        assert_eq!(flags & 0x0080, 0x0080); // RA
        assert_eq!(flags & 0x000f, 3); // NXDOMAIN
        assert_eq!(u16::from_be_bytes([buf[6], buf[7]]), 0); // ANCOUNT
    }

    #[test]
    fn dns_qname_compression_is_rejected() {
        let query = [
            0x12, 0x34, 0x01, 0x00, // id + flags
            0x00, 0x01, 0x00, 0x00, // qdcount=1
            0x00, 0x00, 0x00, 0x00, // an/ns/ar = 0
            0xc0, 0x0c, // QNAME: compression pointer (invalid in our minimal parser)
            0x00, 0x01, 0x00, 0x01, // QTYPE=A, QCLASS=IN
        ];
        assert_eq!(
            parse_single_query(&query).unwrap_err(),
            PacketError::Unsupported("compressed DNS QNAME")
        );
    }

    #[test]
    fn dns_qname_over_255_bytes_is_rejected() {
        let mut query = Vec::new();
        query.extend_from_slice(&0x1234u16.to_be_bytes());
        query.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
        query.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        query.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
        query.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        query.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

        // 4 labels × (1 length byte + 63 payload bytes) + terminator = 257 bytes of QNAME.
        for _ in 0..4 {
            query.push(63);
            query.extend_from_slice(&[b'a'; 63]);
        }
        query.push(0);
        query.extend_from_slice(&1u16.to_be_bytes()); // QTYPE=A
        query.extend_from_slice(&1u16.to_be_bytes()); // QCLASS=IN

        assert_eq!(
            parse_single_query(&query).unwrap_err(),
            PacketError::Malformed("DNS QNAME too long")
        );
    }
}
