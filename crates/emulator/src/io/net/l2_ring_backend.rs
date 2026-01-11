use aero_ipc::ring::{PopError, PushError, RingBuffer};

use super::{L2TunnelBackend, L2TunnelBackendStats, NetworkBackend};

/// Minimal interface required by [`L2TunnelRingBackend`] for exchanging variable-length
/// Ethernet frames with an external transport.
///
/// This is intentionally separate from `aero_ipc::ring::*` so the backend can be unit tested
/// on the host and used with the `SharedArrayBuffer` implementation in WASM builds.
pub trait FrameRing {
    /// Attempt to enqueue a frame payload into the ring.
    ///
    /// Returns `true` on success; `false` if the ring is full or the payload does not fit.
    fn try_push(&self, payload: &[u8]) -> bool;

    /// Attempt to dequeue a frame payload from the ring.
    fn try_pop(&self) -> Option<Vec<u8>>;
}

impl FrameRing for RingBuffer {
    fn try_push(&self, payload: &[u8]) -> bool {
        match RingBuffer::try_push(self, payload) {
            Ok(()) => true,
            Err(PushError::Full | PushError::TooLarge) => false,
        }
    }

    fn try_pop(&self) -> Option<Vec<u8>> {
        match RingBuffer::try_pop(self) {
            Ok(v) => Some(v),
            Err(PopError::Empty | PopError::Corrupt) => None,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl FrameRing for aero_ipc::wasm::SharedRingBuffer {
    fn try_push(&self, payload: &[u8]) -> bool {
        aero_ipc::wasm::SharedRingBuffer::try_push(self, payload)
    }

    fn try_pop(&self) -> Option<Vec<u8>> {
        let v = aero_ipc::wasm::SharedRingBuffer::try_pop(self)?;
        let mut out = vec![0u8; v.length() as usize];
        v.copy_to(&mut out);
        Some(out)
    }
}

/// A ring-buffer-backed L2 tunnel backend for passing raw Ethernet frames between the emulator and
/// an external host transport.
///
/// This backend is a drop-in alternative to [`super::L2TunnelBackend`] for browser/WASM runtimes:
/// the guest and host exchange frames through two lock-free rings:
/// - guest → host (`NET_TX`)
/// - host → guest (`NET_RX`)
///
/// Frames are dropped (and counted) when:
/// - they exceed `max_frame_bytes`
/// - the underlying ring cannot accept the record (full or record doesn't fit)
pub struct L2TunnelRingBackend<TX: FrameRing, RX: FrameRing> {
    tx_ring: TX,
    rx_ring: RX,
    max_frame_bytes: usize,
    stats: L2TunnelBackendStats,
}

impl<TX: FrameRing, RX: FrameRing> L2TunnelRingBackend<TX, RX> {
    pub const DEFAULT_MAX_FRAME_BYTES: usize = L2TunnelBackend::DEFAULT_MAX_FRAME_BYTES;

    pub fn new(tx_ring: TX, rx_ring: RX) -> Self {
        Self::with_max_frame_bytes(tx_ring, rx_ring, Self::DEFAULT_MAX_FRAME_BYTES)
    }

    pub fn with_max_frame_bytes(tx_ring: TX, rx_ring: RX, max_frame_bytes: usize) -> Self {
        Self {
            tx_ring,
            rx_ring,
            max_frame_bytes,
            stats: L2TunnelBackendStats::default(),
        }
    }

    pub fn tx_ring(&self) -> &TX {
        &self.tx_ring
    }

    pub fn rx_ring(&self) -> &RX {
        &self.rx_ring
    }

    pub fn stats(&self) -> L2TunnelBackendStats {
        self.stats
    }
}

impl<TX: FrameRing, RX: FrameRing> NetworkBackend for L2TunnelRingBackend<TX, RX> {
    fn transmit(&mut self, frame: Vec<u8>) {
        if frame.len() > self.max_frame_bytes {
            self.stats.tx_dropped_oversize += 1;
            return;
        }

        if !self.tx_ring.try_push(&frame) {
            self.stats.tx_dropped_full += 1;
            return;
        }

        self.stats.tx_enqueued_frames += 1;
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        loop {
            let frame = self.rx_ring.try_pop()?;
            if frame.len() > self.max_frame_bytes {
                self.stats.rx_dropped_oversize += 1;
                continue;
            }
            self.stats.rx_enqueued_frames += 1;
            return Some(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use aero_ipc::ring;

    use super::*;

    #[test]
    fn transmit_writes_frame_to_ring() {
        let tx_ring = RingBuffer::new(4096);
        let rx_ring = RingBuffer::new(4096);
        let mut backend = L2TunnelRingBackend::new(tx_ring, rx_ring);

        backend.transmit(vec![1, 2, 3, 4]);
        assert_eq!(
            FrameRing::try_pop(backend.tx_ring()).unwrap(),
            vec![1, 2, 3, 4]
        );
        assert!(FrameRing::try_pop(backend.tx_ring()).is_none());

        assert_eq!(
            backend.stats(),
            L2TunnelBackendStats {
                tx_enqueued_frames: 1,
                ..L2TunnelBackendStats::default()
            }
        );
    }

    #[test]
    fn poll_receive_reads_frame_from_ring() {
        let tx_ring = RingBuffer::new(4096);
        let rx_ring = RingBuffer::new(4096);
        let mut backend = L2TunnelRingBackend::new(tx_ring, rx_ring);

        assert!(FrameRing::try_push(backend.rx_ring(), &[9, 8, 7]));

        assert_eq!(backend.poll_receive(), Some(vec![9, 8, 7]));
        assert_eq!(backend.poll_receive(), None);
        assert_eq!(
            backend.stats(),
            L2TunnelBackendStats {
                rx_enqueued_frames: 1,
                ..L2TunnelBackendStats::default()
            }
        );
    }

    #[test]
    fn oversized_frames_are_dropped_and_counted() {
        let tx_ring = RingBuffer::new(4096);
        let rx_ring = RingBuffer::new(4096);
        let mut backend = L2TunnelRingBackend::with_max_frame_bytes(tx_ring, rx_ring, 2);

        backend.transmit(vec![0, 1, 2]);

        assert!(FrameRing::try_push(backend.rx_ring(), &[3, 4, 5]));

        assert!(FrameRing::try_pop(backend.tx_ring()).is_none());
        assert_eq!(backend.poll_receive(), None);

        assert_eq!(
            backend.stats(),
            L2TunnelBackendStats {
                tx_enqueued_frames: 0,
                tx_dropped_oversize: 1,
                tx_dropped_full: 0,
                rx_enqueued_frames: 0,
                rx_dropped_oversize: 1,
                rx_dropped_full: 0,
            }
        );
    }

    #[test]
    fn full_ring_drops_are_counted() {
        let cap = ring::record_size(1);
        let tx_ring = RingBuffer::new(cap);
        let rx_ring = RingBuffer::new(4096);
        let mut backend = L2TunnelRingBackend::new(tx_ring, rx_ring);

        backend.transmit(vec![1]);
        backend.transmit(vec![2]);

        assert_eq!(FrameRing::try_pop(backend.tx_ring()).unwrap(), vec![1]);
        assert!(FrameRing::try_pop(backend.tx_ring()).is_none());

        assert_eq!(
            backend.stats(),
            L2TunnelBackendStats {
                tx_enqueued_frames: 1,
                tx_dropped_oversize: 0,
                tx_dropped_full: 1,
                rx_enqueued_frames: 0,
                rx_dropped_oversize: 0,
                rx_dropped_full: 0,
            }
        );
    }

    #[test]
    fn record_too_large_for_ring_counts_as_full_drop() {
        let cap = ring::record_size(2);
        let tx_ring = RingBuffer::new(cap);
        let rx_ring = RingBuffer::new(4096);
        let mut backend = L2TunnelRingBackend::new(tx_ring, rx_ring);

        // Fits within max_frame_bytes but cannot fit into the ring.
        backend.transmit(vec![0; cap]);
        assert_eq!(
            backend.stats(),
            L2TunnelBackendStats {
                tx_enqueued_frames: 0,
                tx_dropped_oversize: 0,
                tx_dropped_full: 1,
                rx_enqueued_frames: 0,
                rx_dropped_oversize: 0,
                rx_dropped_full: 0,
            }
        );
    }
}
