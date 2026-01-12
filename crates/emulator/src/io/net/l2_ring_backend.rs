//! Compatibility shim for the ring-buffer-backed L2 tunnel network backend.
//!
//! The canonical implementation lives in the shared `aero-net-backend` crate and is re-exported
//! by [`super::ring_backend`]. This module exists to preserve the historical module name
//! introduced during early bring-up.

pub use super::ring_backend::{
    FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats, NetworkBackend,
};
