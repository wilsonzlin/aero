use std::{
    collections::HashMap,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use anyhow::Result;
use trust_dns_resolver::TokioAsyncResolver;

#[derive(Debug, Clone)]
pub struct DnsService {
    resolver: TokioAsyncResolver,
    static_a: HashMap<String, Ipv4Addr>,
    default_ttl_secs: u32,
    max_ttl_secs: u32,
}

impl DnsService {
    pub fn new(
        static_a: HashMap<String, Ipv4Addr>,
        default_ttl_secs: u32,
        max_ttl_secs: u32,
    ) -> Result<Self> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf().unwrap_or_else(|err| {
            tracing::warn!("failed to load system DNS config ({err}); using default resolver config");
            TokioAsyncResolver::tokio(
                trust_dns_resolver::config::ResolverConfig::default(),
                trust_dns_resolver::config::ResolverOpts::default(),
            )
        });
        Ok(Self {
            resolver,
            static_a,
            default_ttl_secs,
            max_ttl_secs,
        })
    }

    pub async fn resolve_ipv4(&self, name: &str) -> Result<(Option<Ipv4Addr>, u32, bool)> {
        let name_norm = name.trim_end_matches('.').to_ascii_lowercase();

        if let Some(ip) = self.static_a.get(&name_norm).copied() {
            // Treat test-mode mappings as authoritative and stable.
            return Ok((Some(ip), self.default_ttl_secs, true));
        }

        match self.resolver.ipv4_lookup(name_norm).await {
            Ok(lookup) => {
                let ip = lookup.iter().next().map(|a| a.0);
                let ttl = ttl_secs_from_valid_until(
                    Some(lookup.valid_until()),
                    self.default_ttl_secs,
                    self.max_ttl_secs,
                );
                Ok((ip, ttl, false))
            }
            Err(_err) => Ok((None, 0, false)),
        }
    }
}

fn ttl_secs_from_valid_until(valid_until: Option<Instant>, default_ttl: u32, max_ttl: u32) -> u32 {
    let now = Instant::now();
    let ttl = valid_until
        .map(|t| t.saturating_duration_since(now))
        .unwrap_or_else(|| Duration::from_secs(default_ttl as u64))
        .as_secs()
        .min(max_ttl as u64) as u32;
    if ttl == 0 {
        default_ttl.min(max_ttl)
    } else {
        ttl
    }
}

