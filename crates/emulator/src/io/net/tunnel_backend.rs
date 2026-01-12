//! Queue-backed L2 tunnel network backend.
//!
//! The implementation lives in the lightweight `aero-net-backend` crate. This module exists to
//! preserve the historical path `emulator::io::net::tunnel_backend`.

pub use aero_net_backend::{L2TunnelBackend, L2TunnelBackendStats, NetworkBackend};
