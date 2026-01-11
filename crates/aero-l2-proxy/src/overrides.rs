use std::{collections::HashMap, net::Ipv4Addr};

use anyhow::{anyhow, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForwardKey {
    pub ip: Ipv4Addr,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct ForwardTarget {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Default)]
pub struct TestOverrides {
    pub dns_a: HashMap<String, Ipv4Addr>,
    pub tcp_forward: HashMap<ForwardKey, ForwardTarget>,
    pub udp_forward: HashMap<ForwardKey, ForwardTarget>,
}

impl TestOverrides {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            dns_a: parse_dns_a_env()?,
            tcp_forward: parse_forward_env("AERO_L2_TCP_FORWARD")?,
            udp_forward: parse_forward_env("AERO_L2_UDP_FORWARD")?,
        })
    }
}

fn parse_dns_a_env() -> Result<HashMap<String, Ipv4Addr>> {
    let Ok(raw) = std::env::var("AERO_L2_DNS_A") else {
        return Ok(HashMap::new());
    };

    let mut out = HashMap::new();
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(out);
    }

    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, ip) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid AERO_L2_DNS_A entry {entry:?}"))?;
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        let ip: Ipv4Addr = ip.trim().parse().with_context(|| {
            format!("invalid IPv4 address in AERO_L2_DNS_A entry {entry:?}")
        })?;
        out.insert(name, ip);
    }

    Ok(out)
}

fn parse_forward_env(var: &str) -> Result<HashMap<ForwardKey, ForwardTarget>> {
    let Ok(raw) = std::env::var(var) else {
        return Ok(HashMap::new());
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(HashMap::new());
    }

    let mut out = HashMap::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (src, dst) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid {var} entry {entry:?}"))?;

        let (src_ip, src_port) = parse_ipv4_socket(src.trim())
            .with_context(|| format!("invalid src addr in {var} entry {entry:?}"))?;

        let (host, port) = parse_host_port(dst.trim())
            .with_context(|| format!("invalid target addr in {var} entry {entry:?}"))?;

        out.insert(
            ForwardKey {
                ip: src_ip,
                port: src_port,
            },
            ForwardTarget { host, port },
        );
    }

    Ok(out)
}

fn parse_ipv4_socket(s: &str) -> Result<(Ipv4Addr, u16)> {
    let (ip, port) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("missing :port in {s:?}"))?;
    let ip: Ipv4Addr = ip.parse().with_context(|| format!("invalid IPv4 addr {ip:?}"))?;
    let port: u16 = port.parse().with_context(|| format!("invalid port {port:?}"))?;
    Ok((ip, port))
}

fn parse_host_port(s: &str) -> Result<(String, u16)> {
    if let Some(rest) = s.strip_prefix('[') {
        let Some((host, rest)) = rest.split_once(']') else {
            return Err(anyhow!("missing closing bracket in {s:?}"));
        };
        let port = rest
            .strip_prefix(':')
            .ok_or_else(|| anyhow!("missing :port suffix in {s:?}"))?;
        let port: u16 = port.parse().with_context(|| format!("invalid port {port:?}"))?;
        return Ok((host.to_string(), port));
    }

    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("missing :port in {s:?}"))?;
    let port: u16 = port.parse().with_context(|| format!("invalid port {port:?}"))?;
    Ok((host.to_string(), port))
}

