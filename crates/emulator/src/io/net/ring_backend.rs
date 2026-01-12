//! Compatibility shim for the ring-buffer-backed L2 tunnel network backend.
//!
//! The canonical implementation lives in the shared `aero-net-backend` crate so both the native
//! emulator and other integration layers (e.g. `aero-machine`) can use the NET_TX/NET_RX ring
//! backend without depending on `crates/emulator`.

pub use aero_net_backend::{FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats};

