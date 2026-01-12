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
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if self
            .deny_domains
            .iter()
            .any(|suffix| domain_matches(&name, suffix))
        {
            return false;
        }
        if !self.allow_domains.is_empty() {
            return self
                .allow_domains
                .iter()
                .any(|suffix| domain_matches(&name, suffix));
        }
        true
    }
}

fn domain_matches(name: &str, suffix: &str) -> bool {
    if name == suffix {
        return true;
    }
    let Some(rest) = name.strip_suffix(suffix) else {
        return false;
    };
    rest.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::domain_matches;

    #[test]
    fn domain_matches_requires_suffix_boundary() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(domain_matches("sub.example.com", "example.com"));
        assert!(!domain_matches("notexample.com", "example.com"));
        assert!(!domain_matches("example.com.evil", "example.com"));
        assert!(!domain_matches("evilcom", "com"));
        assert!(domain_matches("example.com", "com"));
    }
}
