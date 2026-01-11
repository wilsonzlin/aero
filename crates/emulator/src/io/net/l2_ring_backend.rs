//! Compatibility shim for the ring-buffer-backed L2 tunnel network backend.
//!
//! The implementation lives in [`super::ring_backend`]; this module exists to preserve the
//! historical module name introduced during early bring-up.

pub use super::ring_backend::{FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats};
