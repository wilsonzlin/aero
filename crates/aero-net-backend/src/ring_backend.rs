use std::sync::Arc;

use aero_ipc::ring::{PopError, PushError};

use crate::NetworkBackend;

/// Minimal abstraction over an Aero IPC ring buffer suitable for framing raw Ethernet payloads.
pub trait FrameRing {
    fn capacity_bytes(&self) -> usize;
    fn try_push(&self, payload: &[u8]) -> Result<(), PushError>;
    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError>;
}

impl<T: FrameRing + ?Sized> FrameRing for &T {
    fn capacity_bytes(&self) -> usize {
        <T as FrameRing>::capacity_bytes(&**self)
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        <T as FrameRing>::try_push(&**self, payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec(&**self)
    }
}

impl<T: FrameRing + ?Sized> FrameRing for Box<T> {
    fn capacity_bytes(&self) -> usize {
        <T as FrameRing>::capacity_bytes(&**self)
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        <T as FrameRing>::try_push(&**self, payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec(&**self)
    }
}

impl FrameRing for aero_ipc::ring::RingBuffer {
    fn capacity_bytes(&self) -> usize {
        aero_ipc::ring::RingBuffer::capacity_bytes(self)
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        aero_ipc::ring::RingBuffer::try_push(self, payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        aero_ipc::ring::RingBuffer::try_pop(self)
    }
}

impl<T: FrameRing + ?Sized> FrameRing for Arc<T> {
    fn capacity_bytes(&self) -> usize {
        <T as FrameRing>::capacity_bytes(&**self)
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        <T as FrameRing>::try_push(&**self, payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec(&**self)
    }
}

#[cfg(target_arch = "wasm32")]
impl FrameRing for aero_ipc::wasm::SharedRingBuffer {
    fn capacity_bytes(&self) -> usize {
        aero_ipc::wasm::SharedRingBuffer::capacity_bytes(self) as usize
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        let cap = aero_ipc::wasm::SharedRingBuffer::capacity_bytes(self) as usize;
        if aero_ipc::ring::record_size(payload.len()) > cap {
            return Err(PushError::TooLarge);
        }

        if aero_ipc::wasm::SharedRingBuffer::try_push(self, payload) {
            Ok(())
        } else {
            Err(PushError::Full)
        }
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        match aero_ipc::wasm::SharedRingBuffer::try_pop(self) {
            Some(buf) => {
                let mut out = vec![0u8; buf.length() as usize];
                buf.copy_to(&mut out);
                Ok(out)
            }
            None => Err(PopError::Empty),
        }
    }
}

/// Stats for [`L2TunnelRingBackend`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct L2TunnelRingBackendStats {
    pub tx_pushed_frames: u64,
    pub tx_dropped_oversize: u64,
    pub tx_dropped_full: u64,

    pub rx_popped_frames: u64,
    pub rx_dropped_oversize: u64,
    pub rx_corrupt: u64,
}

/// A ring-buffer-backed variant of [`crate::L2TunnelBackend`].
///
/// This backend bridges raw Ethernet frames to/from Aero IPC ring buffers so the
/// WASM emulator can exchange frames with a JS forwarder without `postMessage`
/// copies.
///
/// ## WASM/browser usage (NET_TX / NET_RX)
///
/// In the browser runtime, the tunnel forwarder lives in JS (`web/src/net/l2TunnelForwarder.ts`)
/// and exchanges raw Ethernet frames with the emulator worker via the `ioIpcSab` AIPC queues. The
/// worker can open those rings by `kind` and construct this backend directly:
///
/// ```ignore
/// use aero_ipc::layout::io_ipc_queue_kind;
/// use aero_net_backend::L2TunnelRingBackend;
///
/// // `io_ipc_sab` is the SharedArrayBuffer created by `createIoIpcSab()`.
/// let net_tx = aero_ipc::wasm::open_ring_by_kind(io_ipc_sab.clone(), io_ipc_queue_kind::NET_TX, 0)?;
/// let net_rx = aero_ipc::wasm::open_ring_by_kind(io_ipc_sab, io_ipc_queue_kind::NET_RX, 0)?;
/// let backend = L2TunnelRingBackend::new(net_tx, net_rx);
/// # Ok::<(), wasm_bindgen::JsValue>(())
/// ```
pub struct L2TunnelRingBackend<TX, RX> {
    tx: TX,
    rx: RX,
    max_frame_bytes: usize,
    stats: L2TunnelRingBackendStats,
    rx_broken: bool,
}

impl<TX: FrameRing, RX: FrameRing> L2TunnelRingBackend<TX, RX> {
    pub const DEFAULT_MAX_FRAME_BYTES: usize = 2048;

    pub fn new(tx: TX, rx: RX) -> Self {
        Self::with_max_frame_bytes(tx, rx, Self::DEFAULT_MAX_FRAME_BYTES)
    }

    pub fn with_max_frame_bytes(tx: TX, rx: RX, max_frame_bytes: usize) -> Self {
        Self {
            tx,
            rx,
            max_frame_bytes,
            stats: L2TunnelRingBackendStats::default(),
            rx_broken: false,
        }
    }

    pub fn tx_ring(&self) -> &TX {
        &self.tx
    }

    pub fn rx_ring(&self) -> &RX {
        &self.rx
    }

    pub fn stats(&self) -> L2TunnelRingBackendStats {
        self.stats
    }
}

impl<TX: FrameRing, RX: FrameRing> NetworkBackend for L2TunnelRingBackend<TX, RX> {
    fn transmit(&mut self, frame: Vec<u8>) {
        if frame.len() > self.max_frame_bytes {
            self.stats.tx_dropped_oversize += 1;
            return;
        }

        match self.tx.try_push(&frame) {
            Ok(()) => self.stats.tx_pushed_frames += 1,
            Err(PushError::Full) => self.stats.tx_dropped_full += 1,
            Err(PushError::TooLarge) => self.stats.tx_dropped_oversize += 1,
        }
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        if self.rx_broken {
            return None;
        }

        loop {
            match self.rx.try_pop_vec() {
                Ok(frame) => {
                    if frame.len() > self.max_frame_bytes {
                        self.stats.rx_dropped_oversize += 1;
                        continue;
                    }

                    self.stats.rx_popped_frames += 1;
                    return Some(frame);
                }
                Err(PopError::Empty) => return None,
                Err(PopError::Corrupt) => {
                    self.stats.rx_corrupt += 1;
                    self.rx_broken = true;
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use aero_ipc::ring::{PopError, RingBuffer};

    use super::*;

    #[test]
    fn transmit_pushes_fifo_to_tx_ring() {
        let tx = Arc::new(RingBuffer::new(64));
        let rx = Arc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::new(tx.clone(), rx);

        backend.transmit(vec![1, 2, 3]);
        backend.transmit(vec![4, 5]);

        assert_eq!(tx.try_pop(), Ok(vec![1, 2, 3]));
        assert_eq!(tx.try_pop(), Ok(vec![4, 5]));
        assert_eq!(tx.try_pop(), Err(PopError::Empty));
    }

    #[test]
    fn poll_receive_pops_fifo_from_rx_ring() {
        let tx = Arc::new(RingBuffer::new(64));
        let rx = Arc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::new(tx, rx.clone());

        rx.try_push(&[9]).unwrap();
        rx.try_push(&[8, 7]).unwrap();

        assert_eq!(backend.poll_receive(), Some(vec![9]));
        assert_eq!(backend.poll_receive(), Some(vec![8, 7]));
        assert_eq!(backend.poll_receive(), None);
    }

    #[test]
    fn oversized_tx_is_dropped_and_not_written_to_ring() {
        let tx = Arc::new(RingBuffer::new(64));
        let rx = Arc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::with_max_frame_bytes(tx.clone(), rx, 2);

        backend.transmit(vec![0, 1, 2]);

        assert_eq!(
            backend.stats(),
            L2TunnelRingBackendStats {
                tx_pushed_frames: 0,
                tx_dropped_oversize: 1,
                tx_dropped_full: 0,
                rx_popped_frames: 0,
                rx_dropped_oversize: 0,
                rx_corrupt: 0,
            }
        );
        assert_eq!(tx.try_pop(), Err(PopError::Empty));
    }

    #[test]
    fn tx_full_drops_are_counted() {
        // Capacity is exactly one 1-byte record (4-byte length + padding to 4-byte align).
        let cap = aero_ipc::ring::record_size(1);
        let tx = Arc::new(RingBuffer::new(cap));
        let rx = Arc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::new(tx.clone(), rx);

        backend.transmit(vec![1]);
        backend.transmit(vec![2]);

        assert_eq!(
            backend.stats(),
            L2TunnelRingBackendStats {
                tx_pushed_frames: 1,
                tx_dropped_oversize: 0,
                tx_dropped_full: 1,
                rx_popped_frames: 0,
                rx_dropped_oversize: 0,
                rx_corrupt: 0,
            }
        );

        assert_eq!(tx.try_pop(), Ok(vec![1]));
        assert_eq!(tx.try_pop(), Err(PopError::Empty));
    }

    #[test]
    fn oversized_rx_frames_are_dropped_and_do_not_block_later_frames() {
        let tx = Arc::new(RingBuffer::new(64));
        let rx = Arc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::with_max_frame_bytes(tx, rx.clone(), 2);

        rx.try_push(&[0, 1, 2]).unwrap();
        rx.try_push(&[9]).unwrap();

        assert_eq!(backend.poll_receive(), Some(vec![9]));
        assert_eq!(backend.poll_receive(), None);

        assert_eq!(
            backend.stats(),
            L2TunnelRingBackendStats {
                tx_pushed_frames: 0,
                tx_dropped_oversize: 0,
                tx_dropped_full: 0,
                rx_popped_frames: 1,
                rx_dropped_oversize: 1,
                rx_corrupt: 0,
            }
        );
    }

    struct CorruptOnceRing {
        calls: Cell<u32>,
    }

    impl CorruptOnceRing {
        fn new() -> Self {
            Self {
                calls: Cell::new(0),
            }
        }

        fn calls(&self) -> u32 {
            self.calls.get()
        }
    }

    impl FrameRing for CorruptOnceRing {
        fn capacity_bytes(&self) -> usize {
            64
        }

        fn try_push(&self, _payload: &[u8]) -> Result<(), PushError> {
            Ok(())
        }

        fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
            let calls = self.calls.get();
            self.calls.set(calls + 1);

            if calls == 0 {
                Err(PopError::Corrupt)
            } else {
                Ok(vec![0x42])
            }
        }
    }

    #[test]
    fn poll_receive_marks_rx_broken_on_corrupt_and_stops_polling_ring() {
        let tx = Arc::new(RingBuffer::new(64));
        let rx = Arc::new(CorruptOnceRing::new());
        let mut backend = L2TunnelRingBackend::new(tx, rx.clone());

        assert_eq!(backend.poll_receive(), None);
        assert_eq!(rx.calls(), 1);
        assert_eq!(
            backend.stats(),
            L2TunnelRingBackendStats {
                tx_pushed_frames: 0,
                tx_dropped_oversize: 0,
                tx_dropped_full: 0,
                rx_popped_frames: 0,
                rx_dropped_oversize: 0,
                rx_corrupt: 1,
            }
        );

        // Corrupt RX permanently breaks the ring; the backend should not attempt further pops.
        assert_eq!(backend.poll_receive(), None);
        assert_eq!(rx.calls(), 1);
        assert_eq!(backend.stats().rx_corrupt, 1);
    }
}
