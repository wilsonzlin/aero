use std::collections::BTreeMap;

use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self([a, b, c, d])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpLease {
    pub ip: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub lease_time_secs: u32,
    // Deterministic time base: tick counter from VM start.
    pub acquired_at_tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NatProtocol {
    Tcp = 6,
    Udp = 17,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NatKey {
    pub proto: NatProtocol,
    pub inside_ip: Ipv4Addr,
    pub inside_port: u16,
    pub outside_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatValue {
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,
    pub last_seen_tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyConnStatus {
    Connected = 1,
    Disconnected = 2,
    Reconnecting = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConnection {
    pub id: u32,
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,
    pub status: ProxyConnStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpRestorePolicy {
    /// Drop all active TCP proxy connections on restore.
    Drop,
    /// Preserve connection IDs and endpoints, but mark them as needing reconnection.
    Reconnect,
}

/// Legacy host-side (user-space) network stack state.
///
/// This snapshot state predates the current browser-friendly user-space network stack
/// implementation in [`aero-net-stack`](https://crates.io/crates/aero-net-stack) and does **not**
/// match its internal state.
///
/// New code should snapshot [`aero_net_stack::NetworkStack`] directly (device id `b"NETS"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyNetworkStackState {
    pub mac_addr: [u8; 6],
    pub dhcp_lease: Option<DhcpLease>,
    pub nat: BTreeMap<NatKey, NatValue>,

    pub next_conn_id: u32,
    pub tcp_proxy_conns: BTreeMap<u32, ProxyConnection>,
}

#[deprecated(
    note = "Legacy NAT-based network snapshot state (device id b\"NETL\"). \
            Snapshot aero_net_stack::NetworkStack (device id b\"NETS\") instead."
)]
pub type NetworkStackState = LegacyNetworkStackState;

impl Default for LegacyNetworkStackState {
    fn default() -> Self {
        Self {
            mac_addr: [0; 6],
            dhcp_lease: None,
            nat: BTreeMap::new(),
            next_conn_id: 1,
            tcp_proxy_conns: BTreeMap::new(),
        }
    }
}

impl LegacyNetworkStackState {
    pub fn open_tcp_connection(&mut self, remote_ip: Ipv4Addr, remote_port: u16) -> u32 {
        let id = self.next_conn_id;
        self.next_conn_id += 1;
        self.tcp_proxy_conns.insert(
            id,
            ProxyConnection {
                id,
                remote_ip,
                remote_port,
                status: ProxyConnStatus::Connected,
            },
        );
        id
    }

    pub fn apply_tcp_restore_policy(&mut self, policy: TcpRestorePolicy) {
        match policy {
            TcpRestorePolicy::Drop => {
                self.tcp_proxy_conns.clear();
            }
            TcpRestorePolicy::Reconnect => {
                for conn in self.tcp_proxy_conns.values_mut() {
                    conn.status = ProxyConnStatus::Reconnecting;
                }
            }
        }
    }
}

impl IoSnapshot for LegacyNetworkStackState {
    // NOTE: This is intentionally *not* `b"NETS"` to avoid colliding with the canonical
    // `aero_net_stack::NetworkStack` snapshot encoding.
    const DEVICE_ID: [u8; 4] = *b"NETL";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_MAC: u16 = 1;
        const TAG_DHCP: u16 = 2;
        const TAG_NAT: u16 = 3;
        const TAG_NEXT_CONN_ID: u16 = 4;
        const TAG_TCP_CONNS: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_MAC, self.mac_addr.to_vec());
        w.field_u32(TAG_NEXT_CONN_ID, self.next_conn_id);

        if let Some(lease) = &self.dhcp_lease {
            let bytes = Encoder::new()
                .bytes(&lease.ip.0)
                .bytes(&lease.gateway.0)
                .bytes(&lease.netmask.0)
                .u32(lease.lease_time_secs)
                .u64(lease.acquired_at_tick)
                .finish();
            w.field_bytes(TAG_DHCP, bytes);
        }

        let mut nat = Encoder::new().u32(self.nat.len() as u32);
        for (k, v) in &self.nat {
            nat = nat
                .u8(k.proto as u8)
                .bytes(&k.inside_ip.0)
                .u16(k.inside_port)
                .u16(k.outside_port)
                .bytes(&v.remote_ip.0)
                .u16(v.remote_port)
                .u64(v.last_seen_tick);
        }
        w.field_bytes(TAG_NAT, nat.finish());

        let mut conns = Encoder::new().u32(self.tcp_proxy_conns.len() as u32);
        for (id, conn) in &self.tcp_proxy_conns {
            conns = conns
                .u32(*id)
                .bytes(&conn.remote_ip.0)
                .u16(conn.remote_port)
                .u8(conn.status as u8);
        }
        w.field_bytes(TAG_TCP_CONNS, conns.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_MAC: u16 = 1;
        const TAG_DHCP: u16 = 2;
        const TAG_NAT: u16 = 3;
        const TAG_NEXT_CONN_ID: u16 = 4;
        const TAG_TCP_CONNS: u16 = 5;

        const MAX_NAT_ENTRIES: usize = 65_536;
        const MAX_TCP_PROXY_CONNS: usize = 65_536;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(mac) = r.bytes(TAG_MAC) {
            if mac.len() != 6 {
                return Err(SnapshotError::InvalidFieldEncoding("mac"));
            }
            self.mac_addr.copy_from_slice(mac);
        }
        self.next_conn_id = r.u32(TAG_NEXT_CONN_ID)?.unwrap_or(1).max(1);

        self.dhcp_lease = if let Some(buf) = r.bytes(TAG_DHCP) {
            let mut d = Decoder::new(buf);
            let ip = {
                let b = d.bytes(4)?;
                Ipv4Addr([b[0], b[1], b[2], b[3]])
            };
            let gateway = {
                let b = d.bytes(4)?;
                Ipv4Addr([b[0], b[1], b[2], b[3]])
            };
            let netmask = {
                let b = d.bytes(4)?;
                Ipv4Addr([b[0], b[1], b[2], b[3]])
            };
            let lease_time_secs = d.u32()?;
            let acquired_at_tick = d.u64()?;
            d.finish()?;
            Some(DhcpLease {
                ip,
                gateway,
                netmask,
                lease_time_secs,
                acquired_at_tick,
            })
        } else {
            None
        };

        self.nat.clear();
        if let Some(buf) = r.bytes(TAG_NAT) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_NAT_ENTRIES {
                return Err(SnapshotError::InvalidFieldEncoding("too many nat entries"));
            }
            for _ in 0..count {
                let proto = match d.u8()? {
                    6 => NatProtocol::Tcp,
                    17 => NatProtocol::Udp,
                    _ => return Err(SnapshotError::InvalidFieldEncoding("nat proto")),
                };
                let inside_ip = {
                    let b = d.bytes(4)?;
                    Ipv4Addr([b[0], b[1], b[2], b[3]])
                };
                let inside_port = d.u16()?;
                let outside_port = d.u16()?;
                let remote_ip = {
                    let b = d.bytes(4)?;
                    Ipv4Addr([b[0], b[1], b[2], b[3]])
                };
                let remote_port = d.u16()?;
                let last_seen_tick = d.u64()?;

                self.nat.insert(
                    NatKey {
                        proto,
                        inside_ip,
                        inside_port,
                        outside_port,
                    },
                    NatValue {
                        remote_ip,
                        remote_port,
                        last_seen_tick,
                    },
                );
            }
            d.finish()?;
        }

        self.tcp_proxy_conns.clear();
        if let Some(buf) = r.bytes(TAG_TCP_CONNS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_TCP_PROXY_CONNS {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "too many tcp proxy connections",
                ));
            }
            for _ in 0..count {
                let id = d.u32()?;
                let remote_ip = {
                    let b = d.bytes(4)?;
                    Ipv4Addr([b[0], b[1], b[2], b[3]])
                };
                let remote_port = d.u16()?;
                let _status = d.u8()?;

                // Always restore as disconnected; policy decides whether to reconnect or drop.
                self.tcp_proxy_conns.insert(
                    id,
                    ProxyConnection {
                        id,
                        remote_ip,
                        remote_port,
                        status: ProxyConnStatus::Disconnected,
                    },
                );
            }
            d.finish()?;
        }

        Ok(())
    }
}
