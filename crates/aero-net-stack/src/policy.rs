#![forbid(unsafe_code)]

use core::net::Ipv4Addr;

/// A minimal IPv4 CIDR implementation (e.g. `10.0.0.0/8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpCidr {
    network: Ipv4Addr,
    prefix_len: u8,
}

impl IpCidr {
    pub const fn new(network: Ipv4Addr, prefix_len: u8) -> Self {
        Self {
            network,
            prefix_len,
        }
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let prefix_len = self.prefix_len.min(32);
        let mask = if prefix_len == 0 {
            0u32
        } else {
            u32::MAX << (32 - prefix_len)
        };
        let net = u32::from(self.network) & mask;
        let ip = u32::from(ip) & mask;
        net == ip
    }
}

/// Security policy for outbound guest traffic.
///
/// This is intentionally small and deterministic: it only needs to cover the typical "default deny
/// unless explicitly enabled" browser UX, plus allow/deny lists for demos/enterprise use.
#[derive(Debug, Clone, Default)]
pub struct HostPolicy {
    /// When false, outbound traffic to non-local addresses is rejected (TCP reset, DNS NXDOMAIN,
    /// UDP dropped).
    pub enabled: bool,

    /// If non-empty, only IPs matching at least one CIDR are allowed.
    pub allow_ips: Vec<IpCidr>,
    /// If an IP matches any CIDR here, it is denied.
    pub deny_ips: Vec<IpCidr>,

    /// If non-empty, only DNS names matching one of these suffixes are allowed.
    ///
    /// Entries should be in lower-case without a trailing dot (e.g. `"example.com"`). Matching is
    /// done on `name == suffix || name.ends_with("." + suffix)`.
    pub allow_domains: Vec<String>,
    /// DNS name suffix denylist. See `allow_domains` for format and matching semantics.
    pub deny_domains: Vec<String>,
}

impl HostPolicy {
    pub fn allows_ip(&self, ip: Ipv4Addr) -> bool {
        if self.deny_ips.iter().any(|cidr| cidr.contains(ip)) {
            return false;
        }
        if !self.allow_ips.is_empty() {
            return self.allow_ips.iter().any(|cidr| cidr.contains(ip));
        }
        true
    }

    pub fn allows_domain(&self, name: &str) -> bool {
        let name = name.trim().trim_end_matches('.');
        if self
            .deny_domains
            .iter()
            .any(|suffix| domain_matches(name, suffix))
        {
            return false;
        }
        if !self.allow_domains.is_empty() {
            return self
                .allow_domains
                .iter()
                .any(|suffix| domain_matches(name, suffix));
        }
        true
    }
}

fn domain_matches(name: &str, suffix: &str) -> bool {
    let suffix = suffix.trim().trim_end_matches('.');
    if suffix.is_empty() {
        return false;
    }

    if bytes_eq_ignore_ascii_case(name.as_bytes(), suffix.as_bytes()) {
        return true;
    }

    if name.len() <= suffix.len() {
        return false;
    };

    let name_bytes = name.as_bytes();
    let suffix_bytes = suffix.as_bytes();
    let start = name_bytes.len() - suffix_bytes.len();
    if name_bytes[start - 1] != b'.' {
        return false;
    }

    bytes_eq_ignore_ascii_case(&name_bytes[start..], suffix_bytes)
}

fn bytes_eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{domain_matches, HostPolicy};

    #[test]
    fn domain_matches_requires_suffix_boundary_and_is_case_insensitive() {
        assert!(domain_matches("Example.COM", "example.com"));
        assert!(domain_matches("sub.Example.COM", "example.com"));
        assert!(!domain_matches("notexample.com", "example.com"));
        assert!(!domain_matches("example.com.evil", "example.com"));
        assert!(!domain_matches("evilcom", "com"));
        assert!(domain_matches("example.COM", "COM"));
        assert!(domain_matches("example.com", "com."));
    }

    #[test]
    fn host_policy_allows_domain_trims_name_and_honors_lists() {
        let policy = HostPolicy {
            enabled: true,
            allow_domains: vec!["example.com".to_string()],
            deny_domains: vec!["bad.example.com".to_string()],
            ..Default::default()
        };

        assert!(policy.allows_domain("sub.example.com."));
        assert!(!policy.allows_domain("notexample.com"));
        assert!(!policy.allows_domain("bad.example.com"));
        assert!(!policy.allows_domain("sub.bad.example.com"));
        assert!(!policy.allows_domain("evil.com"));
    }
}
