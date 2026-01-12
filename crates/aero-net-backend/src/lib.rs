//! Shared network backend primitives used by both the native emulator and the `wasm32` runtime.
//!
//! This crate is intentionally minimal: it deals exclusively with raw Ethernet frames (`Vec<u8>`)
//! and provides backends suitable for bridging emulated NIC models to host glue.
//!
//! This crate exists so consumers that only need these backends do not need to depend on the
//! heavyweight `emulator` crate (e.g. the canonical machine).
#![forbid(unsafe_code)]

pub mod ring_backend;
pub mod tunnel_backend;

pub use aero_ipc::ring::{PopError, PushError};
pub use ring_backend::{FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats};
pub use tunnel_backend::{L2TunnelBackend, L2TunnelBackendStats};

/// Network backend to bridge frames between emulated NICs and the host network stack.
///
/// This is intentionally minimal: devices only need a way to transmit Ethernet frames to the
/// outside world. Incoming frames are delivered via device-specific queues (e.g. RX rings).
pub trait NetworkBackend {
    /// Transmit a guest → host Ethernet frame.
    fn transmit(&mut self, frame: Vec<u8>);

    /// Poll for a host → guest Ethernet frame.
    ///
    /// Backends may return immediate responses (ARP/DHCP/DNS, etc.) when the guest transmits,
    /// allowing round-trips within a single emulation tick when used by a pump.
    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// If this backend is backed by the shared `NET_TX`/`NET_RX` rings, return its statistics.
    ///
    /// Backends that are not ring-backed should return `None` (default).
    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        None
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for Box<T> {
    fn transmit(&mut self, frame: Vec<u8>) {
        <T as NetworkBackend>::transmit(&mut **self, frame);
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        <T as NetworkBackend>::l2_ring_stats(&**self)
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        <T as NetworkBackend>::poll_receive(&mut **self)
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for &mut T {
    fn transmit(&mut self, frame: Vec<u8>) {
        <T as NetworkBackend>::transmit(&mut **self, frame);
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        <T as NetworkBackend>::l2_ring_stats(&**self)
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        <T as NetworkBackend>::poll_receive(&mut **self)
    }
}

impl NetworkBackend for () {
    fn transmit(&mut self, _frame: Vec<u8>) {}
}

impl<B: NetworkBackend> NetworkBackend for Option<B> {
    fn transmit(&mut self, frame: Vec<u8>) {
        if let Some(backend) = self.as_mut() {
            backend.transmit(frame);
        }
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.as_ref().and_then(|backend| backend.l2_ring_stats())
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.as_mut().and_then(|backend| backend.poll_receive())
    }
}
