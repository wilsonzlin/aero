use std::{collections::HashSet, net::Ipv4Addr, sync::LazyLock};

use anyhow::{Context, Result};

use aero_net_stack::IpCidr;

static DEFAULT_DENY_IPV4: LazyLock<Vec<IpCidr>> = LazyLock::new(|| {
    use Ipv4Addr as Ip;
    vec![
        // "This host on this network"
        IpCidr::new(Ip::new(0, 0, 0, 0), 8),
        // RFC1918 private networks
        IpCidr::new(Ip::new(10, 0, 0, 0), 8),
        IpCidr::new(Ip::new(172, 16, 0, 0), 12),
        IpCidr::new(Ip::new(192, 168, 0, 0), 16),
        // Shared address space (RFC6598)
        IpCidr::new(Ip::new(100, 64, 0, 0), 10),
        // Loopback
        IpCidr::new(Ip::new(127, 0, 0, 0), 8),
        // Link-local
        IpCidr::new(Ip::new(169, 254, 0, 0), 16),
        // IETF protocol assignments (RFC6890)
        IpCidr::new(Ip::new(192, 0, 0, 0), 24),
        // TEST-NET-1/2/3 (RFC5737)
        IpCidr::new(Ip::new(192, 0, 2, 0), 24),
        IpCidr::new(Ip::new(198, 51, 100, 0), 24),
        IpCidr::new(Ip::new(203, 0, 113, 0), 24),
        // Benchmarking (RFC2544)
        IpCidr::new(Ip::new(198, 18, 0, 0), 15),
        // Multicast (RFC5771)
        IpCidr::new(Ip::new(224, 0, 0, 0), 4),
        // Reserved / broadcast
        IpCidr::new(Ip::new(240, 0, 0, 0), 4),
    ]
});

#[derive(Debug, Clone)]
pub struct EgressPolicy {
    allow_private_ips: bool,
    allowed_tcp_ports: Option<HashSet<u16>>,
    allowed_udp_ports: Option<HashSet<u16>>,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
}

impl EgressPolicy {
    pub fn from_env() -> Result<Self> {
        let allow_private_ips = matches!(
            std::env::var("AERO_L2_ALLOW_PRIVATE_IPS")
                .as_deref()
                .map(str::trim)
                .unwrap_or("0"),
            "1" | "true" | "yes" | "on"
        );

        let allowed_tcp_ports =
            parse_port_allowlist_env("AERO_L2_ALLOWED_TCP_PORTS").context("tcp port allowlist")?;
        let allowed_udp_ports =
            parse_port_allowlist_env("AERO_L2_ALLOWED_UDP_PORTS").context("udp port allowlist")?;

        let allowed_domains = parse_domain_list_env("AERO_L2_ALLOWED_DOMAINS");
        let blocked_domains = parse_domain_list_env("AERO_L2_BLOCKED_DOMAINS");

        Ok(Self {
            allow_private_ips,
            allowed_tcp_ports,
            allowed_udp_ports,
            allowed_domains,
            blocked_domains,
        })
    }

    pub fn allow_private_ips(&self) -> bool {
        self.allow_private_ips
    }

    pub fn allows_ip(&self, ip: Ipv4Addr) -> bool {
        if self.allow_private_ips {
            return true;
        }
        !DEFAULT_DENY_IPV4.iter().any(|cidr| cidr.contains(ip))
    }

    pub fn allows_tcp_port(&self, port: u16) -> bool {
        match &self.allowed_tcp_ports {
            Some(allow) => allow.contains(&port),
            None => true,
        }
    }

    pub fn allows_udp_port(&self, port: u16) -> bool {
        match &self.allowed_udp_ports {
            Some(allow) => allow.contains(&port),
            None => true,
        }
    }

    pub fn allows_domain(&self, name: &str) -> bool {
        let name = normalize_domain(name);
        if self
            .blocked_domains
            .iter()
            .any(|suffix| domain_matches(&name, suffix))
        {
            return false;
        }
        if !self.allowed_domains.is_empty() {
            return self
                .allowed_domains
                .iter()
                .any(|suffix| domain_matches(&name, suffix));
        }
        true
    }
}

fn parse_port_allowlist_env(var: &str) -> Result<Option<HashSet<u16>>> {
    let raw = match std::env::var(var) {
        Ok(v) => v,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    // When set, deny by default unless explicitly allowed.
    let mut out = HashSet::new();
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Some(out));
    }

    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let port: u16 = item
            .parse()
            .with_context(|| format!("invalid {var} entry {item:?}"))?;
        out.insert(port);
    }
    Ok(Some(out))
}

fn parse_domain_list_env(var: &str) -> Vec<String> {
    let Ok(raw) = std::env::var(var) else {
        return Vec::new();
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| normalize_domain(s))
        .collect()
}

fn normalize_domain(name: &str) -> String {
    name.trim_end_matches('.').trim().to_ascii_lowercase()
}

fn domain_matches(name: &str, suffix: &str) -> bool {
    if name == suffix {
        return true;
    }
    name.ends_with(&format!(".{suffix}"))
}
