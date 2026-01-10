//! Aero's browser-constrained user-space networking stack.
//!
//! This crate is intentionally "pure" networking logic: it parses Ethernet/IP packets coming from
//! the guest NIC and emits actions that the host (browser) must implement via WebSocket/WebRTC and
//! fetch (DoH). The host then feeds events back into the stack, which generates inbound Ethernet
//! frames for the guest.

#![forbid(unsafe_code)]

mod checksum;
pub mod packet;
mod policy;
mod stack;

pub use policy::{HostPolicy, IpCidr};
pub use stack::{
    Action, DnsResolved, Millis, NetworkStack, StackConfig, TcpProxyEvent, UdpProxyEvent,
    UdpTransport,
};
