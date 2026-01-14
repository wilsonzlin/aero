use std::cell::RefCell;
use std::sync::{Arc, Mutex, RwLock};

use aero_ipc::ring::{PopError, PushError};
use aero_l2_protocol::L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD;

use crate::NetworkBackend;

/// Maximum number of RX ring records to pop per [`NetworkBackend::poll_receive`] call.
///
/// This bounds worst-case host work if the ring contains a large number of oversized frames (which
/// are dropped without being returned to the NIC model).
pub const MAX_RX_POPS_PER_POLL: usize = 64;

/// Minimal abstraction over an Aero IPC ring buffer suitable for framing raw Ethernet payloads.
pub trait FrameRing {
    fn capacity_bytes(&self) -> usize;
    fn try_push(&self, payload: &[u8]) -> Result<(), PushError>;
    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError>;

    /// Pop a frame, but drop any record whose payload length exceeds `max_len` and return
    /// [`PopError::TooLarge`].
    ///
    /// Implementors may override this to avoid allocating the full record when it is going to be
    /// dropped anyway (e.g. `aero_ipc::ring::RingBuffer::try_pop_capped`).
    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        let frame = self.try_pop_vec()?;
        if frame.len() > max_len {
            return Err(PopError::TooLarge {
                len: u32::try_from(frame.len()).unwrap_or(u32::MAX),
                max: u32::try_from(max_len).unwrap_or(u32::MAX),
            });
        }
        Ok(frame)
    }
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

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec_capped(&**self, max_len)
    }
}

impl<T: FrameRing + ?Sized> FrameRing for &mut T {
    fn capacity_bytes(&self) -> usize {
        <T as FrameRing>::capacity_bytes(&**self)
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        <T as FrameRing>::try_push(&**self, payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec(&**self)
    }

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec_capped(&**self, max_len)
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

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec_capped(&**self, max_len)
    }
}

impl<T: FrameRing + ?Sized> FrameRing for std::rc::Rc<T> {
    fn capacity_bytes(&self) -> usize {
        <T as FrameRing>::capacity_bytes(&**self)
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        <T as FrameRing>::try_push(&**self, payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec(&**self)
    }

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec_capped(&**self, max_len)
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

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        aero_ipc::ring::RingBuffer::try_pop_capped(self, max_len)
    }
}

impl<T: FrameRing + ?Sized> FrameRing for RefCell<T> {
    fn capacity_bytes(&self) -> usize {
        self.borrow().capacity_bytes()
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        self.borrow().try_push(payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        self.borrow().try_pop_vec()
    }

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        self.borrow().try_pop_vec_capped(max_len)
    }
}

impl<T: FrameRing + ?Sized> FrameRing for Mutex<T> {
    fn capacity_bytes(&self) -> usize {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .capacity_bytes()
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_push(payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_pop_vec()
    }

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_pop_vec_capped(max_len)
    }
}

impl<T: FrameRing + ?Sized> FrameRing for RwLock<T> {
    fn capacity_bytes(&self) -> usize {
        self.read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .capacity_bytes()
    }

    fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        self.write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_push(payload)
    }

    fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
        self.write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_pop_vec()
    }

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        self.write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_pop_vec_capped(max_len)
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

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        <T as FrameRing>::try_pop_vec_capped(&**self, max_len)
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
        aero_ipc::wasm::SharedRingBuffer::try_pop_vec(self)
    }

    fn try_pop_vec_capped(&self, max_len: usize) -> Result<Vec<u8>, PopError> {
        aero_ipc::wasm::SharedRingBuffer::try_pop_vec_capped(self, max_len)
    }
}

/// Stats for [`L2TunnelRingBackend`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct L2TunnelRingBackendStats {
    pub tx_pushed_frames: u64,
    pub tx_pushed_bytes: u64,
    pub tx_dropped_oversize: u64,
    pub tx_dropped_oversize_bytes: u64,
    pub tx_dropped_full: u64,
    pub tx_dropped_full_bytes: u64,

    pub rx_popped_frames: u64,
    pub rx_popped_bytes: u64,
    pub rx_dropped_oversize: u64,
    pub rx_dropped_oversize_bytes: u64,
    pub rx_corrupt: u64,
    /// Whether the RX ring has entered a permanent broken state due to corruption.
    ///
    /// When this is `true`, [`L2TunnelRingBackend::poll_receive`] will stop attempting to pop from
    /// the ring and will always return `None`.
    pub rx_broken: bool,
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
}

impl<TX: FrameRing, RX: FrameRing> L2TunnelRingBackend<TX, RX> {
    pub const DEFAULT_MAX_FRAME_BYTES: usize = L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD;

    pub fn new(tx: TX, rx: RX) -> Self {
        Self::with_max_frame_bytes(tx, rx, Self::DEFAULT_MAX_FRAME_BYTES)
    }

    pub fn with_max_frame_bytes(tx: TX, rx: RX, max_frame_bytes: usize) -> Self {
        Self {
            tx,
            rx,
            max_frame_bytes,
            stats: L2TunnelRingBackendStats::default(),
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
        let frame_len = frame.len() as u64;
        if frame.len() > self.max_frame_bytes {
            self.stats.tx_dropped_oversize += 1;
            self.stats.tx_dropped_oversize_bytes += frame_len;
            return;
        }

        match self.tx.try_push(&frame) {
            Ok(()) => {
                self.stats.tx_pushed_frames += 1;
                self.stats.tx_pushed_bytes += frame_len;
            }
            Err(PushError::Full) => {
                self.stats.tx_dropped_full += 1;
                self.stats.tx_dropped_full_bytes += frame_len;
            }
            Err(PushError::TooLarge) => {
                self.stats.tx_dropped_oversize += 1;
                self.stats.tx_dropped_oversize_bytes += frame_len;
            }
        }
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        Some(self.stats())
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        if self.stats.rx_broken {
            return None;
        }

        for _ in 0..MAX_RX_POPS_PER_POLL {
            match self.rx.try_pop_vec_capped(self.max_frame_bytes) {
                Ok(frame) => {
                    let frame_len = frame.len() as u64;
                    if frame.len() > self.max_frame_bytes {
                        self.stats.rx_dropped_oversize += 1;
                        self.stats.rx_dropped_oversize_bytes += frame_len;
                        continue;
                    }
                    self.stats.rx_popped_frames += 1;
                    self.stats.rx_popped_bytes += frame_len;
                    return Some(frame);
                }
                Err(PopError::Empty) => return None,
                Err(PopError::TooLarge { len, .. }) => {
                    self.stats.rx_dropped_oversize += 1;
                    self.stats.rx_dropped_oversize_bytes += u64::from(len);
                    continue;
                }
                Err(PopError::Corrupt) => {
                    self.stats.rx_corrupt += 1;
                    self.stats.rx_broken = true;
                    return None;
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex, RwLock};

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
    fn l2_ring_stats_is_available_through_network_backend_trait_object() {
        let tx = Arc::new(RingBuffer::new(64));
        let rx = Arc::new(RingBuffer::new(64));

        // Mirror the access pattern used by higher-level machine code (e.g. `PcMachine`), which
        // typically stores the backend as `Option<Box<dyn NetworkBackend>>`.
        let mut backend: Option<Box<dyn NetworkBackend>> =
            Some(Box::new(L2TunnelRingBackend::new(tx, rx)));

        assert_eq!(
            backend.l2_ring_stats(),
            Some(L2TunnelRingBackendStats::default())
        );

        backend.as_mut().unwrap().transmit(vec![1, 2, 3]);
        assert_eq!(backend.l2_ring_stats().unwrap().tx_pushed_frames, 1);
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
                tx_dropped_oversize: 1,
                tx_dropped_oversize_bytes: 3,
                ..Default::default()
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
                tx_pushed_bytes: 1,
                tx_dropped_full: 1,
                tx_dropped_full_bytes: 1,
                ..Default::default()
            }
        );

        assert_eq!(tx.try_pop(), Ok(vec![1]));
        assert_eq!(tx.try_pop(), Err(PopError::Empty));
    }

    #[test]
    fn tx_too_large_for_ring_is_counted_as_oversize() {
        // Ring can only fit a single 1-byte record; any larger record should return PushError::TooLarge.
        let cap = aero_ipc::ring::record_size(1);
        let tx = Arc::new(RingBuffer::new(cap));
        let rx = Arc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::new(tx.clone(), rx);

        // Frame is within backend max_frame_bytes, but larger than the ring's capacity.
        backend.transmit(vec![0u8; 9]);

        assert_eq!(
            backend.stats(),
            L2TunnelRingBackendStats {
                tx_dropped_oversize: 1,
                tx_dropped_oversize_bytes: 9,
                ..Default::default()
            }
        );
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
                rx_popped_frames: 1,
                rx_popped_bytes: 1,
                rx_dropped_oversize: 1,
                rx_dropped_oversize_bytes: 3,
                ..Default::default()
            }
        );
    }

    struct OversizeRxRing {
        calls: Cell<u32>,
        ok_until: u32,
        frame_len: usize,
    }

    impl OversizeRxRing {
        fn new(ok_until: u32, frame_len: usize) -> Self {
            Self {
                calls: Cell::new(0),
                ok_until,
                frame_len,
            }
        }

        fn calls(&self) -> u32 {
            self.calls.get()
        }
    }

    impl FrameRing for OversizeRxRing {
        fn capacity_bytes(&self) -> usize {
            64
        }

        fn try_push(&self, _payload: &[u8]) -> Result<(), PushError> {
            Ok(())
        }

        fn try_pop_vec(&self) -> Result<Vec<u8>, PopError> {
            let calls = self.calls.get();
            if calls >= self.ok_until {
                return Err(PopError::Empty);
            }

            self.calls.set(calls + 1);
            Ok(vec![0u8; self.frame_len])
        }
    }

    #[test]
    fn poll_receive_is_bounded_when_dropping_oversized_frames() {
        let tx = Arc::new(RingBuffer::new(64));
        let rx = Rc::new(OversizeRxRing::new((MAX_RX_POPS_PER_POLL + 10) as u32, 3));

        let mut backend = L2TunnelRingBackend::with_max_frame_bytes(tx, rx.clone(), 2);

        assert_eq!(backend.poll_receive(), None);
        assert_eq!(rx.calls(), MAX_RX_POPS_PER_POLL as u32);
        assert_eq!(
            backend.stats(),
            L2TunnelRingBackendStats {
                rx_dropped_oversize: MAX_RX_POPS_PER_POLL as u64,
                rx_dropped_oversize_bytes: (MAX_RX_POPS_PER_POLL as u64) * 3,
                ..Default::default()
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
        // `CorruptOnceRing` uses interior mutability via `Cell` and is not `Sync`, so use `Rc`
        // instead of `Arc` to avoid implying cross-thread usage in this unit test.
        let rx = Rc::new(CorruptOnceRing::new());
        let mut backend = L2TunnelRingBackend::new(tx, rx.clone());

        assert_eq!(backend.poll_receive(), None);
        assert_eq!(rx.calls(), 1);
        assert_eq!(
            backend.stats(),
            L2TunnelRingBackendStats {
                rx_corrupt: 1,
                rx_broken: true,
                ..Default::default()
            }
        );

        // Corrupt RX permanently breaks the ring; the backend should not attempt further pops.
        assert_eq!(backend.poll_receive(), None);
        assert_eq!(rx.calls(), 1);
        assert_eq!(backend.stats().rx_corrupt, 1);
        assert!(backend.stats().rx_broken);
    }

    #[test]
    fn backend_works_with_boxed_rings_via_frame_ring_impl() {
        let tx = Box::new(RingBuffer::new(64));
        let rx = Box::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::new(tx, rx);

        backend.transmit(vec![1, 2, 3]);
        assert_eq!(backend.tx_ring().try_pop_vec(), Ok(vec![1, 2, 3]));

        backend.rx_ring().try_push(&[9]).unwrap();
        assert_eq!(backend.poll_receive(), Some(vec![9]));
    }

    #[test]
    fn backend_works_with_rc_rings_via_frame_ring_impl() {
        let tx = Rc::new(RingBuffer::new(64));
        let rx = Rc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::new(tx.clone(), rx.clone());

        backend.transmit(vec![4, 5]);
        assert_eq!(tx.try_pop(), Ok(vec![4, 5]));

        rx.try_push(&[8]).unwrap();
        assert_eq!(backend.poll_receive(), Some(vec![8]));
    }

    #[test]
    fn backend_works_with_refcell_rings_via_frame_ring_impl() {
        let tx = Rc::new(RefCell::new(RingBuffer::new(64)));
        let rx = Rc::new(RefCell::new(RingBuffer::new(64)));
        let mut backend = L2TunnelRingBackend::new(tx.clone(), rx.clone());

        backend.transmit(vec![4, 5]);
        assert_eq!(tx.borrow().try_pop(), Ok(vec![4, 5]));

        rx.borrow().try_push(&[8]).unwrap();
        assert_eq!(backend.poll_receive(), Some(vec![8]));
    }

    #[test]
    fn backend_works_with_mutex_rings_via_frame_ring_impl() {
        let tx = Arc::new(Mutex::new(RingBuffer::new(64)));
        let rx = Arc::new(Mutex::new(RingBuffer::new(64)));
        let mut backend = L2TunnelRingBackend::new(tx, rx);

        backend.transmit(vec![1, 2, 3]);
        assert_eq!(backend.tx_ring().try_pop_vec(), Ok(vec![1, 2, 3]));

        backend.rx_ring().try_push(&[9]).unwrap();
        assert_eq!(backend.poll_receive(), Some(vec![9]));
    }

    #[test]
    fn backend_works_with_rwlock_rings_via_frame_ring_impl() {
        let tx = Arc::new(RwLock::new(RingBuffer::new(64)));
        let rx = Arc::new(RwLock::new(RingBuffer::new(64)));
        let mut backend = L2TunnelRingBackend::new(tx, rx);

        backend.transmit(vec![1, 2, 3]);
        assert_eq!(backend.tx_ring().try_pop_vec(), Ok(vec![1, 2, 3]));

        backend.rx_ring().try_push(&[9]).unwrap();
        assert_eq!(backend.poll_receive(), Some(vec![9]));
    }
}
