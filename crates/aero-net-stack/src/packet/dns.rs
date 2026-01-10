#![forbid(unsafe_code)]

use super::ParseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsType {
    A = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsResponseCode {
    NoError = 0,
    FormatError = 1,
    ServerFailure = 2,
    NameError = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsQuestion {
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsMessage {
    pub id: u16,
    pub flags: u16,
    pub questions: Vec<DnsQuestion>,
}

impl DnsMessage {
    pub fn parse_query(buf: &[u8]) -> Result<Self, ParseError> {
        if buf.len() < 12 {
            return Err(ParseError::Truncated);
        }
        let id = u16::from_be_bytes([buf[0], buf[1]]);
        let flags = u16::from_be_bytes([buf[2], buf[3]]);
        let qdcount = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        if qdcount == 0 {
            return Err(ParseError::Invalid("no DNS questions"));
        }
        let mut offset = 12usize;
        let mut questions = Vec::with_capacity(qdcount);
        for _ in 0..qdcount {
            let (name, next) = decode_name(buf, offset)?;
            offset = next;
            if offset + 4 > buf.len() {
                return Err(ParseError::Truncated);
            }
            let qtype = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
            let qclass = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]);
            offset += 4;
            questions.push(DnsQuestion {
                name,
                qtype,
                qclass,
            });
        }
        Ok(Self {
            id,
            flags,
            questions,
        })
    }

    pub fn build_a_response(
        id: u16,
        rd: bool,
        question_name: &str,
        addr: Option<[u8; 4]>,
        ttl_secs: u32,
        rcode: DnsResponseCode,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&id.to_be_bytes());
        let mut flags: u16 = 0;
        flags |= 1 << 15; // QR
        if rd {
            flags |= 1 << 8;
        }
        flags |= 1 << 7; // RA
        flags |= rcode as u16;
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        out.extend_from_slice(&(if addr.is_some() { 1u16 } else { 0u16 }).to_be_bytes()); // ANCOUNT
        out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

        let question_offset = out.len();
        encode_name(question_name, &mut out);
        out.extend_from_slice(&(DnsType::A as u16).to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // IN

        if let Some(addr) = addr {
            // NAME pointer to question name.
            out.extend_from_slice(
                &((0b1100_0000u16 << 8) | (question_offset as u16)).to_be_bytes(),
            );
            out.extend_from_slice(&(DnsType::A as u16).to_be_bytes());
            out.extend_from_slice(&1u16.to_be_bytes()); // IN
            out.extend_from_slice(&ttl_secs.to_be_bytes());
            out.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
            out.extend_from_slice(&addr);
        }
        out
    }
}

fn encode_name(name: &str, out: &mut Vec<u8>) {
    let trimmed = name.trim_end_matches('.');
    if trimmed.is_empty() {
        out.push(0);
        return;
    }
    for label in trimmed.split('.') {
        let label_bytes = label.as_bytes();
        out.push(label_bytes.len() as u8);
        out.extend_from_slice(label_bytes);
    }
    out.push(0);
}

fn decode_name(buf: &[u8], mut offset: usize) -> Result<(String, usize), ParseError> {
    let mut labels = Vec::new();
    let mut jumped = false;
    let mut next_offset: Option<usize> = None;
    let mut seen = 0usize;
    loop {
        if offset >= buf.len() {
            return Err(ParseError::Truncated);
        }
        if seen > buf.len() {
            return Err(ParseError::Invalid("DNS name pointer loop"));
        }
        seen += 1;
        let len = buf[offset];
        if len == 0 {
            offset += 1;
            break;
        }
        if len & 0b1100_0000 == 0b1100_0000 {
            if offset + 1 >= buf.len() {
                return Err(ParseError::Truncated);
            }
            if !jumped {
                next_offset = Some(offset + 2);
            }
            let ptr = (((len & 0b0011_1111) as u16) << 8) | buf[offset + 1] as u16;
            offset = ptr as usize;
            jumped = true;
            continue;
        }
        let len = len as usize;
        offset += 1;
        if offset + len > buf.len() {
            return Err(ParseError::Truncated);
        }
        labels.push(
            core::str::from_utf8(&buf[offset..offset + len])
                .map_err(|_| ParseError::Invalid("DNS label utf8"))?
                .to_string(),
        );
        offset += len;
    }

    let next = if jumped {
        next_offset.ok_or(ParseError::Invalid("DNS pointer without end"))?
    } else {
        offset
    };
    Ok((labels.join("."), next))
}
