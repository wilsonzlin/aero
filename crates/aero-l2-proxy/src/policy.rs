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

#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    allow_private_ips: bool,
    allowed_tcp_ports: Option<HashSet<u16>>,
    allowed_udp_ports: Option<HashSet<u16>>,
    /// If non-empty, outbound DNS/TCP destinations must match at least one of these suffixes.
    ///
    /// Matching is ASCII case-insensitive and boundary-aware:
    /// `name == suffix || name.ends_with("." + suffix)`.
    allowed_domains: Vec<String>,
    /// DNS name suffix denylist. See `allowed_domains` for matching semantics.
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
        let name = name.trim().trim_end_matches('.');
        if self
            .blocked_domains
            .iter()
            .any(|suffix| domain_matches(name, suffix))
        {
            return false;
        }
        if !self.allowed_domains.is_empty() {
            return self
                .allowed_domains
                .iter()
                .any(|suffix| domain_matches(name, suffix));
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
        .map(normalize_domain)
        .collect()
}

fn normalize_domain(name: &str) -> String {
    name.trim_end_matches('.').trim().to_ascii_lowercase()
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
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prior }
        }

        fn unset(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn base_env() -> (
        EnvVarGuard,
        EnvVarGuard,
        EnvVarGuard,
        EnvVarGuard,
        EnvVarGuard,
    ) {
        (
            EnvVarGuard::unset("AERO_L2_ALLOW_PRIVATE_IPS"),
            EnvVarGuard::unset("AERO_L2_ALLOWED_TCP_PORTS"),
            EnvVarGuard::unset("AERO_L2_ALLOWED_UDP_PORTS"),
            EnvVarGuard::unset("AERO_L2_ALLOWED_DOMAINS"),
            EnvVarGuard::unset("AERO_L2_BLOCKED_DOMAINS"),
        )
    }

    #[test]
    fn allow_private_ips_overrides_default_deny_list() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let (_allow_private, _tcp, _udp, _allowed_domains, _blocked_domains) = base_env();

        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(
            !policy.allows_ip(Ipv4Addr::new(10, 0, 0, 1)),
            "expected private IPs to be denied by default"
        );

        let _allow_private = EnvVarGuard::set("AERO_L2_ALLOW_PRIVATE_IPS", "1");
        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(
            policy.allows_ip(Ipv4Addr::new(10, 0, 0, 1)),
            "expected allow_private_ips to permit private IPs"
        );
    }

    #[test]
    fn tcp_port_allowlist_is_deny_by_default_when_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let (_allow_private, _tcp, _udp, _allowed_domains, _blocked_domains) = base_env();

        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(policy.allows_tcp_port(80));

        // When set to the empty string, the allowlist is enabled but contains zero ports, so all
        // ports should be denied.
        let _tcp = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "");
        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(!policy.allows_tcp_port(80));

        // When set to a list, only those ports are allowed.
        let _tcp = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "80,443");
        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(policy.allows_tcp_port(80));
        assert!(policy.allows_tcp_port(443));
        assert!(!policy.allows_tcp_port(22));
    }

    #[test]
    fn udp_port_allowlist_is_deny_by_default_when_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let (_allow_private, _tcp, _udp, _allowed_domains, _blocked_domains) = base_env();

        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(policy.allows_udp_port(53));

        // When set to the empty string, the allowlist is enabled but contains zero ports, so all
        // ports should be denied.
        let _udp = EnvVarGuard::set("AERO_L2_ALLOWED_UDP_PORTS", "");
        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(!policy.allows_udp_port(53));

        // When set to a list, only those ports are allowed.
        let _udp = EnvVarGuard::set("AERO_L2_ALLOWED_UDP_PORTS", "53,123");
        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(policy.allows_udp_port(53));
        assert!(policy.allows_udp_port(123));
        assert!(!policy.allows_udp_port(9999));
    }

    #[test]
    fn invalid_port_allowlist_entry_is_rejected() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let (_allow_private, _tcp, _udp, _allowed_domains, _blocked_domains) = base_env();

        let _tcp = EnvVarGuard::set("AERO_L2_ALLOWED_TCP_PORTS", "nope");
        let err = EgressPolicy::from_env().expect_err("expected invalid port list");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("AERO_L2_ALLOWED_TCP_PORTS") && msg.contains("tcp port allowlist"),
            "expected error to mention env var and include context, got {msg:?}"
        );
    }

    #[test]
    fn invalid_udp_port_allowlist_entry_is_rejected() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let (_allow_private, _tcp, _udp, _allowed_domains, _blocked_domains) = base_env();

        let _udp = EnvVarGuard::set("AERO_L2_ALLOWED_UDP_PORTS", "nope");
        let err = EgressPolicy::from_env().expect_err("expected invalid port list");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("AERO_L2_ALLOWED_UDP_PORTS") && msg.contains("udp port allowlist"),
            "expected error to mention env var and include context, got {msg:?}"
        );
    }

    #[test]
    fn domain_allow_and_block_lists_use_suffix_matching() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let (_allow_private, _tcp, _udp, _allowed_domains, _blocked_domains) = base_env();

        // Default: allow all domains.
        let policy = EgressPolicy::from_env().expect("policy from env");
        assert!(policy.allows_domain("example.com"));

        // Allowlist enabled: only matching suffixes pass.
        {
            let _allowed_domains = EnvVarGuard::set("AERO_L2_ALLOWED_DOMAINS", "Example.COM");
            let policy = EgressPolicy::from_env().expect("policy from env");
            assert!(policy.allows_domain("example.com"));
            assert!(policy.allows_domain("sub.example.com."));
            assert!(
                !policy.allows_domain("notexample.com"),
                "expected boundary-aware suffix matching"
            );
            assert!(!policy.allows_domain("other.test"));

            // Blocklist overrides allowlist: blocked domains should be rejected even if the domain
            // is otherwise permitted by the allowlist.
            let _blocked_domains = EnvVarGuard::set("AERO_L2_BLOCKED_DOMAINS", "example.com");
            let policy = EgressPolicy::from_env().expect("policy from env");
            assert!(!policy.allows_domain("example.com"));
            assert!(!policy.allows_domain("sub.example.com"));
            // Allowlist is still in effect for unrelated domains.
            assert!(!policy.allows_domain("other.test"));
        }

        // Without an allowlist, a blocklist should only reject the matching suffixes.
        {
            let _allowed_domains = EnvVarGuard::unset("AERO_L2_ALLOWED_DOMAINS");
            let _blocked_domains = EnvVarGuard::set("AERO_L2_BLOCKED_DOMAINS", "example.com");
            let policy = EgressPolicy::from_env().expect("policy from env");
            assert!(!policy.allows_domain("example.com"));
            assert!(!policy.allows_domain("sub.example.com"));
            assert!(policy.allows_domain("other.test"));
        }

        // More specific blocklist entries should not block siblings.
        {
            let _allowed_domains = EnvVarGuard::set("AERO_L2_ALLOWED_DOMAINS", "example.com");
            let _blocked_domains = EnvVarGuard::set("AERO_L2_BLOCKED_DOMAINS", "bad.example.com");
            let policy = EgressPolicy::from_env().expect("policy from env");
            assert!(policy.allows_domain("example.com"));
            assert!(!policy.allows_domain("bad.example.com"));
            assert!(!policy.allows_domain("sub.bad.example.com"));
            assert!(policy.allows_domain("good.example.com"));
        }
    }
}
