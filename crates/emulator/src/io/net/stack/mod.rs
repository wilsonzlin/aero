//! In-emulator user-space network stack.
//!
//! This module is a thin wrapper around [`aero_net_stack`], which contains the canonical
//! browser-friendly networking implementation. The stack itself is "pure" logic that emits
//! host actions (TCP/UDP proxy + DNS resolve) and consumes host events, producing Ethernet frames
//! for the guest NIC.

pub mod backend;

pub use aero_net_stack::{
    Action, DnsResolved, HostPolicy, IpCidr, Millis, NetworkStack, StackConfig, TcpProxyEvent,
    UdpProxyEvent, UdpTransport,
};

pub use backend::NetStackBackend;

