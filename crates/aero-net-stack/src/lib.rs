//! Aero's browser-constrained user-space networking stack.
//!
//! This crate is intentionally "pure" networking logic: it parses Ethernet/IP packets coming from
//! the guest NIC and emits actions that the host (browser) must implement via WebSocket/WebRTC and
//! fetch (DoH). The host then feeds events back into the stack, which generates inbound Ethernet
//! frames for the guest.
//!
//! ## Resource limits
//!
//! When used on a public-facing proxy, the guest is untrusted and can attempt to trigger unbounded
//! memory growth (e.g. by opening many TCP connections, buffering payload before the proxy tunnel is
//! established, or issuing many DNS requests). [`StackConfig`] includes a set of limits that are
//! enforced by [`NetworkStack`] with conservative defaults:
//!
//! - `max_tcp_connections`: 1024
//! - `max_buffered_tcp_bytes_per_conn`: 256 KiB
//! - `max_pending_dns`: 1024
//! - `max_dns_cache_entries`: 10,000 (FIFO eviction)

#![forbid(unsafe_code)]

pub mod packet;
mod policy;
pub mod snapshot;
mod stack;

pub use policy::{HostPolicy, IpCidr};
pub use snapshot::{
    DnsCacheEntrySnapshot, NetworkStackSnapshotState, TcpConnectionSnapshot, TcpConnectionStatus,
    TcpRestorePolicy,
};
pub use stack::{
    Action, DnsResolved, Millis, NetworkStack, StackConfig, TcpProxyEvent, UdpProxyEvent,
    UdpTransport,
};
