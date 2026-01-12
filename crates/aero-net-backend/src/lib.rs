//! Shared network backend primitives used by both the native emulator and the `wasm32` runtime.
//!
//! This crate is intentionally minimal: it deals exclusively with raw Ethernet frames (`Vec<u8>`)
//! and provides backends suitable for bridging emulated NIC models to host glue.
//!
//! In particular it includes a ring-buffer-backed L2 tunnel backend intended for the
//! browser/runtime NET_TX / NET_RX SharedArrayBuffer path.
//!
//! This crate exists so consumers that only need these backends do not need to depend on the
//! heavyweight `emulator` crate.
#![forbid(unsafe_code)]

pub mod ring_backend;
pub mod tunnel_backend;

pub use ring_backend::{FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats};
pub use tunnel_backend::{L2TunnelBackend, L2TunnelBackendStats};

/// Network backend to bridge frames between emulated NICs and the host network stack.
///
/// This is intentionally minimal: devices only need a way to transmit Ethernet frames to the
/// outside world. Incoming frames are delivered via device-specific queues (e.g. RX rings).
pub trait NetworkBackend {
    fn transmit(&mut self, frame: Vec<u8>);

    /// Poll for a host → guest Ethernet frame.
    ///
    /// NIC models like the E1000 and virtio-net can call this during their poll loop to allow a
    /// user-space network stack backend to return immediate responses (ARP/DHCP/DNS, etc.) in the
    /// same tick.
    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        None
    }
}
