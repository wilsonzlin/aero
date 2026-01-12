use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use aero_net_backend::{
    FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats, NetworkBackend, PopError, PushError,
};

#[derive(Default)]
struct TestTxRing {
    pushed: RefCell<Vec<Vec<u8>>>,
    push_results: RefCell<VecDeque<Result<(), PushError>>>,
}

impl TestTxRing {
    fn with_push_results(results: Vec<Result<(), PushError>>) -> Self {
        Self {
            pushed: RefCell::new(Vec::new()),
            push_results: RefCell::new(results.into()),
        }
    }

    fn pushed_frames(&self) -> Vec<Vec<u8>> {
        self.pushed.borrow().clone()
    }
}

impl FrameRing for TestTxRing {
    fn capacity_bytes(&self) -> usize {
        usize::MAX
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        self.pushed.borrow_mut().push(payload.to_vec());
        self.push_results.borrow_mut().pop_front().unwrap_or(Ok(()))
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        Err(PopError::Empty)
    }
}

#[derive(Default)]
struct TestRxRing {
    pop_calls: Cell<usize>,
    pop_results: RefCell<VecDeque<Result<Vec<u8>, PopError>>>,
}

impl TestRxRing {
    fn from_results(results: Vec<Result<Vec<u8>, PopError>>) -> Self {
        Self {
            pop_calls: Cell::new(0),
            pop_results: RefCell::new(results.into()),
        }
    }

    fn remaining(&self) -> usize {
        self.pop_results.borrow().len()
    }

    fn pop_calls(&self) -> usize {
        self.pop_calls.get()
    }
}

impl FrameRing for TestRxRing {
    fn capacity_bytes(&self) -> usize {
        usize::MAX
    }

    fn try_push(&self, _payload: &[u8]) -> Result<(), PushError> {
        Ok(())
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        self.pop_calls.set(self.pop_calls.get() + 1);
        self.pop_results
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(PopError::Empty))
    }
}

#[test]
fn ring_backend_transmit_counts_push_and_drop_reasons() {
    let tx =
        TestTxRing::with_push_results(vec![Ok(()), Err(PushError::Full), Err(PushError::TooLarge)]);
    let rx = TestRxRing::default();
    let mut backend = L2TunnelRingBackend::with_max_frame_bytes(tx, rx, 5);

    backend.transmit(vec![1, 2, 3]);
    backend.transmit(vec![4, 5, 6]);
    backend.transmit(vec![7, 8, 9]);

    // Oversized frames are dropped before we even touch the ring.
    backend.transmit(vec![0u8; 6]);

    assert_eq!(
        backend.stats(),
        L2TunnelRingBackendStats {
            tx_pushed_frames: 1,
            tx_dropped_oversize: 2, // one TooLarge from ring, one pre-filtered oversize frame
            tx_dropped_full: 1,
            rx_popped_frames: 0,
            rx_dropped_oversize: 0,
            rx_corrupt: 0,
        }
    );

    // Ensure the pre-filtered oversize frame was not pushed into the ring.
    assert_eq!(
        backend.tx_ring().pushed_frames(),
        vec![vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9]]
    );
}

#[test]
fn ring_backend_poll_receive_drops_oversize_frames_with_bounded_work() {
    // `poll_receive` is allowed to pop/drop at most MAX_RX_POPS_PER_POLL frames in one call to avoid
    // pathological ring contents causing unbounded work.
    let oversize = vec![0u8; 10];
    let valid = vec![1u8, 2, 3];

    let mut results = Vec::new();
    for _ in 0..aero_net_backend::ring_backend::MAX_RX_POPS_PER_POLL {
        results.push(Ok(oversize.clone()));
    }
    results.push(Ok(valid.clone()));

    let tx = TestTxRing::default();
    let rx = TestRxRing::from_results(results);
    let mut backend = L2TunnelRingBackend::with_max_frame_bytes(tx, rx, 3);

    // First call: we hit the pop cap while draining oversize frames, so the valid frame remains.
    assert_eq!(backend.poll_receive(), None);
    assert_eq!(
        backend.stats(),
        L2TunnelRingBackendStats {
            tx_pushed_frames: 0,
            tx_dropped_oversize: 0,
            tx_dropped_full: 0,
            rx_popped_frames: 0,
            rx_dropped_oversize: aero_net_backend::ring_backend::MAX_RX_POPS_PER_POLL as u64,
            rx_corrupt: 0,
        }
    );
    assert_eq!(
        backend.rx_ring().remaining(),
        1,
        "valid frame should remain queued"
    );

    // Second call: the valid frame is delivered.
    assert_eq!(backend.poll_receive(), Some(valid));
    assert_eq!(backend.stats().rx_popped_frames, 1);
    assert_eq!(backend.rx_ring().remaining(), 0);
}

#[test]
fn ring_backend_poll_receive_marks_rx_broken_on_corrupt() {
    let tx = TestTxRing::default();
    let rx = TestRxRing::from_results(vec![Err(PopError::Corrupt), Ok(vec![1])]);
    let mut backend = L2TunnelRingBackend::new(tx, rx);

    assert_eq!(backend.poll_receive(), None);
    assert_eq!(backend.stats().rx_corrupt, 1);
    assert_eq!(backend.rx_ring().pop_calls(), 1);

    // Once corrupt, the backend should stop reading the RX ring.
    assert_eq!(backend.poll_receive(), None);
    assert_eq!(backend.stats().rx_corrupt, 1);
    assert_eq!(backend.rx_ring().pop_calls(), 1);
}
