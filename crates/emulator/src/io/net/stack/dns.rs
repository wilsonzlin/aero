use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct DnsAnswer {
    pub addr: Ipv4Addr,
    pub ttl: u32,
}

pub trait DnsUpstream {
    fn resolve_a(&mut self, name: &str) -> Option<DnsAnswer>;
}

#[derive(Debug)]
struct CacheEntry {
    addr: Ipv4Addr,
    expires_at: Instant,
    ttl: u32,
}

#[derive(Debug)]
pub struct DnsServer<U: DnsUpstream> {
    gateway_ip: Ipv4Addr,
    upstream: U,
    cache: HashMap<String, CacheEntry>,
}

impl<U: DnsUpstream> DnsServer<U> {
    pub fn new(gateway_ip: Ipv4Addr, upstream: U) -> Self {
        Self {
            gateway_ip,
            upstream,
            cache: HashMap::new(),
        }
    }

    /// Handle a UDP DNS query.
    ///
    /// Returns `(response_bytes, cache_hit)`.
    pub fn handle_query(&mut self, query: &[u8]) -> (Option<Vec<u8>>, bool) {
        let parsed = match parse_dns_query(query) {
            Some(p) => p,
            None => return (None, false),
        };
        let mut cache_hit = false;

        let now = Instant::now();
        let key = parsed.name.to_ascii_lowercase();
        let answer = if let Some(entry) = self.cache.get(&key) {
            if entry.expires_at > now {
                cache_hit = true;
                Some(DnsAnswer {
                    addr: entry.addr,
                    ttl: entry.ttl,
                })
            } else {
                None
            }
        } else {
            None
        };

        let answer = if let Some(ans) = answer {
            Some(ans)
        } else {
            let ans = self.upstream.resolve_a(&parsed.name);
            if let Some(ans) = ans {
                let ttl = ans.ttl.max(1);
                self.cache.insert(
                    key,
                    CacheEntry {
                        addr: ans.addr,
                        expires_at: now + Duration::from_secs(ttl as u64),
                        ttl,
                    },
                );
                Some(DnsAnswer { addr: ans.addr, ttl })
            } else {
                None
            }
        };

        let response = build_dns_response(query, &parsed, answer, self.gateway_ip);
        (Some(response), cache_hit)
    }
}

#[derive(Debug)]
struct ParsedQuery<'a> {
    id: u16,
    flags: u16,
    name: String,
    qtype: u16,
    qclass: u16,
    question_bytes: &'a [u8],
}

fn parse_dns_query(query: &[u8]) -> Option<ParsedQuery<'_>> {
    if query.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([query[0], query[1]]);
    let flags = u16::from_be_bytes([query[2], query[3]]);
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    if qdcount != 1 {
        return None;
    }

    let (name, name_end) = parse_name(query, 12)?;
    if name_end + 4 > query.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([query[name_end], query[name_end + 1]]);
    let qclass = u16::from_be_bytes([query[name_end + 2], query[name_end + 3]]);
    let question_end = name_end + 4;

    Some(ParsedQuery {
        id,
        flags,
        name,
        qtype,
        qclass,
        question_bytes: &query[12..question_end],
    })
}

fn parse_name(msg: &[u8], mut offset: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut jumped = false;
    let mut end_offset = offset;
    let mut hops = 0usize;

    loop {
        if hops > 128 {
            return None;
        }
        if offset >= msg.len() {
            return None;
        }
        let len = msg[offset];
        if len == 0 {
            offset += 1;
            if !jumped {
                end_offset = offset;
            }
            break;
        }

        // Compression pointer.
        if (len & 0xC0) == 0xC0 {
            if offset + 1 >= msg.len() {
                return None;
            }
            let ptr = (((len as u16 & 0x3F) << 8) | msg[offset + 1] as u16) as usize;
            if !jumped {
                end_offset = offset + 2;
            }
            offset = ptr;
            jumped = true;
            hops += 1;
            continue;
        }
        if (len & 0xC0) != 0 {
            return None;
        }

        offset += 1;
        let end = offset + len as usize;
        if end > msg.len() {
            return None;
        }
        let label = std::str::from_utf8(&msg[offset..end]).ok()?.to_string();
        labels.push(label);
        offset = end;
        if !jumped {
            end_offset = offset;
        }
        hops += 1;
    }

    Some((labels.join("."), end_offset))
}

fn build_dns_response(
    query: &[u8],
    parsed: &ParsedQuery<'_>,
    answer: Option<DnsAnswer>,
    _gateway_ip: Ipv4Addr,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(&parsed.id.to_be_bytes());

    // Preserve RD from the query (bit 8); set QR + RA.
    let rd = parsed.flags & 0x0100;
    let mut flags = 0x8000 | rd | 0x0080;
    let mut ancount = 0u16;
    let rcode: u16;
    if parsed.qtype != 1 || parsed.qclass != 1 {
        rcode = 4; // NOTIMP
    } else if answer.is_none() {
        rcode = 3; // NXDOMAIN
    } else {
        rcode = 0;
        ancount = 1;
    }
    flags |= rcode;
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&ancount.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // nscount
    out.extend_from_slice(&0u16.to_be_bytes()); // arcount

    // Question section: copy exactly from the query.
    out.extend_from_slice(parsed.question_bytes);

    if let Some(ans) = answer {
        // NAME: pointer to offset 12 where the question name begins in our response.
        out.extend_from_slice(&0xC00Cu16.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // A
        out.extend_from_slice(&1u16.to_be_bytes()); // IN
        out.extend_from_slice(&ans.ttl.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&ans.addr.octets());
    }

    // If the query had trailing bytes (EDNS0, etc.), ignore them for now.
    let _ = query;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockUpstream {
        calls: usize,
    }

    impl DnsUpstream for MockUpstream {
        fn resolve_a(&mut self, name: &str) -> Option<DnsAnswer> {
            self.calls += 1;
            match name {
                "example.com" => Some(DnsAnswer {
                    addr: Ipv4Addr::new(93, 184, 216, 34),
                    ttl: 60,
                }),
                _ => None,
            }
        }
    }

    fn build_query(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x1234u16.to_be_bytes());
        out.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
        out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
        out.extend_from_slice(&0u16.to_be_bytes()); // an
        out.extend_from_slice(&0u16.to_be_bytes()); // ns
        out.extend_from_slice(&0u16.to_be_bytes()); // ar

        for label in name.split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0); // end name
        out.extend_from_slice(&1u16.to_be_bytes()); // A
        out.extend_from_slice(&1u16.to_be_bytes()); // IN
        out
    }

    #[test]
    fn dns_query_response_and_cache() {
        let upstream = MockUpstream::default();
        let mut server = DnsServer::new(Ipv4Addr::new(10, 0, 2, 2), upstream);

        let query = build_query("example.com");
        let (resp, hit) = server.handle_query(&query);
        assert!(!hit);
        let resp = resp.expect("response");

        // Basic header checks: same ID, QR set, one answer.
        assert_eq!(&resp[0..2], &query[0..2]);
        let flags = u16::from_be_bytes([resp[2], resp[3]]);
        assert_ne!(flags & 0x8000, 0);
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 1);

        let (resp2, hit2) = server.handle_query(&query);
        assert!(hit2);
        assert_eq!(resp2.unwrap()[0..], resp[0..]);
    }
}
