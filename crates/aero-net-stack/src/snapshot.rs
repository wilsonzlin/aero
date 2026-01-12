#![forbid(unsafe_code)]

use crate::packet::MacAddr;
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use core::net::Ipv4Addr;

// Snapshots may be loaded from untrusted sources (downloaded files). Keep decoding bounded so
// corrupted snapshots cannot force pathological allocations.
const MAX_DNS_CACHE_ENTRIES: usize = 65_536;
const MAX_DNS_NAME_BYTES: usize = 1024;
const MAX_TCP_CONNECTIONS: usize = 65_536;

/// Restore policy for active guest TCP connections.
///
/// The in-browser NAT stack proxies guest TCP to host-side WebSocket/WebRTC tunnels. Those tunnels
/// are not bit-restorable across snapshots, so active TCP connections must be handled with an
/// explicit restore policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpRestorePolicy {
    /// Drop all active TCP connections on restore.
    ///
    /// This is the most deterministic policy (no timing-dependent reconnect behavior).
    Drop,
    /// Preserve connection bookkeeping (IDs/endpoints) and attempt a best-effort reconnect.
    ///
    /// Note: This cannot guarantee the remote TCP stream is preserved; reconnection establishes a
    /// fresh proxy tunnel and is expected to fail for many real-world protocols (e.g. TLS).
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpConnectionStatus {
    Connected = 1,
    Disconnected = 2,
    Reconnecting = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpConnectionSnapshot {
    pub id: u32,
    pub guest_port: u16,
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,
    pub status: TcpConnectionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsCacheEntrySnapshot {
    pub name: String,
    pub addr: Ipv4Addr,
    pub expires_at_ms: u64,
}

/// Serializable snapshot of the dynamic [`crate::NetworkStack`] state.
///
/// This intentionally excludes static [`crate::StackConfig`] fields; callers are expected to
/// recreate the stack with the desired config and then apply this dynamic state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStackSnapshotState {
    pub guest_mac: Option<MacAddr>,
    pub ip_assigned: bool,
    pub next_tcp_id: u32,
    pub next_dns_id: u32,
    pub ipv4_ident: u16,
    /// Most recently observed internal time (see `NetworkStack::to_internal_now_ms`).
    pub last_now_ms: u64,

    /// DNS cache entries in FIFO order (oldest first).
    pub dns_cache: Vec<DnsCacheEntrySnapshot>,

    /// Best-effort TCP connection bookkeeping.
    ///
    /// This is *not* a full TCP stream snapshot; it stores only connection IDs and endpoints.
    pub tcp_connections: Vec<TcpConnectionSnapshot>,
}

impl Default for NetworkStackSnapshotState {
    fn default() -> Self {
        Self {
            guest_mac: None,
            ip_assigned: false,
            next_tcp_id: 1,
            next_dns_id: 1,
            ipv4_ident: 1,
            last_now_ms: 0,
            dns_cache: Vec::new(),
            tcp_connections: Vec::new(),
        }
    }
}

impl NetworkStackSnapshotState {
    fn encode_string(mut e: Encoder, s: &str) -> Encoder {
        e = e.u32(s.len() as u32);
        e.bytes(s.as_bytes())
    }

    fn decode_string(d: &mut Decoder<'_>) -> SnapshotResult<String> {
        let len = d.u32()? as usize;
        if len > MAX_DNS_NAME_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding("dns name too long"));
        }
        let bytes = d.bytes(len)?;
        std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|_| SnapshotError::InvalidFieldEncoding("dns name utf8"))
    }
}

impl IoSnapshot for NetworkStackSnapshotState {
    const DEVICE_ID: [u8; 4] = *b"NSTK";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_GUEST_MAC: u16 = 1;
        const TAG_IP_ASSIGNED: u16 = 2;
        const TAG_NEXT_TCP_ID: u16 = 3;
        const TAG_NEXT_DNS_ID: u16 = 4;
        const TAG_IPV4_IDENT: u16 = 5;
        const TAG_DNS_CACHE: u16 = 6;
        const TAG_TCP_CONNS: u16 = 7;
        const TAG_LAST_NOW_MS: u16 = 8;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        if let Some(mac) = self.guest_mac {
            w.field_bytes(TAG_GUEST_MAC, mac.0.to_vec());
        }
        w.field_bool(TAG_IP_ASSIGNED, self.ip_assigned);
        w.field_u32(TAG_NEXT_TCP_ID, self.next_tcp_id);
        w.field_u32(TAG_NEXT_DNS_ID, self.next_dns_id);
        w.field_u16(TAG_IPV4_IDENT, self.ipv4_ident);
        w.field_u64(TAG_LAST_NOW_MS, self.last_now_ms);

        // DNS cache: preserve FIFO order.
        let mut dns = Encoder::new().u32(self.dns_cache.len() as u32);
        for entry in &self.dns_cache {
            dns = Self::encode_string(dns, &entry.name);
            dns = dns.bytes(&entry.addr.octets());
            dns = dns.u64(entry.expires_at_ms);
        }
        w.field_bytes(TAG_DNS_CACHE, dns.finish());

        // TCP connections: sort by id for deterministic encoding.
        let mut conns = self.tcp_connections.clone();
        conns.sort_by_key(|c| c.id);

        let mut tcp = Encoder::new().u32(conns.len() as u32);
        for conn in &conns {
            tcp = tcp
                .u32(conn.id)
                .u16(conn.guest_port)
                .bytes(&conn.remote_ip.octets())
                .u16(conn.remote_port)
                .u8(conn.status as u8);
        }
        w.field_bytes(TAG_TCP_CONNS, tcp.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_GUEST_MAC: u16 = 1;
        const TAG_IP_ASSIGNED: u16 = 2;
        const TAG_NEXT_TCP_ID: u16 = 3;
        const TAG_NEXT_DNS_ID: u16 = 4;
        const TAG_IPV4_IDENT: u16 = 5;
        const TAG_DNS_CACHE: u16 = 6;
        const TAG_TCP_CONNS: u16 = 7;
        const TAG_LAST_NOW_MS: u16 = 8;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.guest_mac = if let Some(mac) = r.bytes(TAG_GUEST_MAC) {
            if mac.len() != 6 {
                return Err(SnapshotError::InvalidFieldEncoding("guest_mac"));
            }
            Some(MacAddr([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]))
        } else {
            None
        };

        self.ip_assigned = r.bool(TAG_IP_ASSIGNED)?.unwrap_or(false);
        self.next_tcp_id = r.u32(TAG_NEXT_TCP_ID)?.unwrap_or(1).max(1);
        self.next_dns_id = r.u32(TAG_NEXT_DNS_ID)?.unwrap_or(1).max(1);
        self.ipv4_ident = r.u16(TAG_IPV4_IDENT)?.unwrap_or(1);
        if self.ipv4_ident == 0 {
            self.ipv4_ident = 1;
        }
        self.last_now_ms = r.u64(TAG_LAST_NOW_MS)?.unwrap_or(0);

        self.dns_cache.clear();
        if let Some(buf) = r.bytes(TAG_DNS_CACHE) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_DNS_CACHE_ENTRIES {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "too many dns cache entries",
                ));
            }
            self.dns_cache.reserve(count);
            for _ in 0..count {
                let name = Self::decode_string(&mut d)?;
                let b = d.bytes(4)?;
                let addr = Ipv4Addr::new(b[0], b[1], b[2], b[3]);
                let expires_at_ms = d.u64()?;
                self.dns_cache.push(DnsCacheEntrySnapshot {
                    name,
                    addr,
                    expires_at_ms,
                });
            }
            d.finish()?;
        }

        self.tcp_connections.clear();
        if let Some(buf) = r.bytes(TAG_TCP_CONNS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_TCP_CONNECTIONS {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "too many tcp connections",
                ));
            }
            self.tcp_connections.reserve(count);
            for _ in 0..count {
                let id = d.u32()?;
                let guest_port = d.u16()?;
                let b = d.bytes(4)?;
                let remote_ip = Ipv4Addr::new(b[0], b[1], b[2], b[3]);
                let remote_port = d.u16()?;
                let _status = d.u8()?;

                // Proxy connections are not bit-restorable (WebSocket/WebRTC transports cannot be
                // serialized). Always restore as disconnected; the restore policy decides whether to
                // drop or attempt a best-effort reconnect.
                self.tcp_connections.push(TcpConnectionSnapshot {
                    id,
                    guest_port,
                    remote_ip,
                    remote_port,
                    status: TcpConnectionStatus::Disconnected,
                });
            }
            d.finish()?;
        }

        Ok(())
    }
}
