use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use dashmap::DashMap;

#[derive(Clone, Default)]
pub struct DnsCache {
    inner: Arc<DashMap<String, IpAddr>>,
}

impl DnsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub async fn connect(&self, host: &str, port: u16) -> std::io::Result<tokio::net::TcpStream> {
        let ip = match host.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => {
                if let Some(entry) = self.inner.get(host) {
                    *entry
                } else {
                    let mut addrs = tokio::net::lookup_host((host, port)).await?;
                    let Some(addr) = addrs.next() else {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("DNS lookup for {host} returned no addresses"),
                        ));
                    };
                    let ip = addr.ip();
                    self.inner.insert(host.to_string(), ip);
                    ip
                }
            }
        };

        tokio::net::TcpStream::connect(SocketAddr::new(ip, port)).await
    }
}
