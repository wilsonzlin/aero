//! Compatibility smoke tests for `emulator::io::net` re-exports.
//!
//! NET-BACKEND-001 moved the actual backend implementations into the lightweight
//! `aero-net-backend` crate so other integration layers (e.g. `aero-machine`)
//! can use them without depending on the full `emulator` crate.
//!
//! These tests ensure the historical `emulator::io::net::*` paths remain usable.

use std::sync::Arc;

use aero_ipc::ring::{PopError, RingBuffer};
use emulator::io::net::{
    FrameRing, L2TunnelBackend, L2TunnelBackendStats, L2TunnelRingBackend, L2TunnelRingBackendStats,
    NetworkBackend,
};

#[test]
fn queue_backend_paths_work() {
    let mut backend = L2TunnelBackend::with_limits(2, 2, 3);

    // Guest → host
    backend.transmit(vec![1, 2, 3]);
    backend.transmit(vec![4, 5, 6, 7]); // oversize

    // Host → guest
    backend.push_rx_frame(vec![9, 9, 9]);
    backend.push_rx_frame(vec![8, 8, 8]);
    backend.push_rx_frame(vec![7, 7, 7]); // full

    assert_eq!(backend.drain_tx_frames(), vec![vec![1, 2, 3]]);
    assert_eq!(backend.drain_tx_frames(), Vec::<Vec<u8>>::new());

    assert_eq!(backend.poll_receive(), Some(vec![9, 9, 9]));
    assert_eq!(backend.poll_receive(), Some(vec![8, 8, 8]));
    assert_eq!(backend.poll_receive(), None);

    assert_eq!(
        backend.stats(),
        L2TunnelBackendStats {
            tx_enqueued_frames: 1,
            tx_dropped_oversize: 1,
            tx_dropped_full: 0,
            rx_enqueued_frames: 2,
            rx_dropped_oversize: 0,
            rx_dropped_full: 1,
        }
    );
}

#[test]
fn ring_backend_paths_work() {
    // Make the TX ring intentionally small so we can trigger `tx_dropped_full`.
    // Capacity is exactly one 1-byte record (4-byte length + padding to 4-byte align).
    let cap = aero_ipc::ring::record_size(1);
    let tx = Arc::new(RingBuffer::new(cap));
    let rx = Arc::new(RingBuffer::new(64));
    let mut backend = L2TunnelRingBackend::with_max_frame_bytes(tx.clone(), rx.clone(), 2);

    // Guest → host: first frame fits, second is dropped because ring is full.
    backend.transmit(vec![1]);
    backend.transmit(vec![2]);

    // Host → guest: inject a frame into the RX ring.
    rx.try_push(&[9, 9]).unwrap();

    assert_eq!(tx.try_pop(), Ok(vec![1]));
    assert_eq!(tx.try_pop(), Err(PopError::Empty));
    assert_eq!(backend.poll_receive(), Some(vec![9, 9]));
    assert_eq!(backend.poll_receive(), None);

    assert_eq!(
        backend.stats(),
        L2TunnelRingBackendStats {
            tx_pushed_frames: 1,
            tx_dropped_oversize: 0,
            tx_dropped_full: 1,
            rx_popped_frames: 1,
            rx_dropped_oversize: 0,
            rx_corrupt: 0,
        }
    );
}

#[test]
fn ring_backend_stats_are_available_through_network_backend_trait_object() {
    let tx = Arc::new(RingBuffer::new(64));
    let rx = Arc::new(RingBuffer::new(64));

    let backend: Box<dyn NetworkBackend> = Box::new(L2TunnelRingBackend::new(tx, rx));
    assert_eq!(
        backend.l2_ring_stats(),
        Some(L2TunnelRingBackendStats::default())
    );
}

#[test]
fn traits_are_reexported() {
    fn assert_frame_ring_impl<T: FrameRing>() {}
    fn assert_network_backend_impl<T: NetworkBackend>() {}

    assert_frame_ring_impl::<RingBuffer>();

    struct Dummy;
    impl NetworkBackend for Dummy {
        fn transmit(&mut self, _frame: Vec<u8>) {}
    }
    assert_network_backend_impl::<Dummy>();
}
