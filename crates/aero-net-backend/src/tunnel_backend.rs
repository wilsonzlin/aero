use std::collections::VecDeque;

use crate::NetworkBackend;

/// Stats for [`L2TunnelBackend`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct L2TunnelBackendStats {
    pub tx_enqueued_frames: u64,
    pub tx_dropped_oversize: u64,
    pub tx_dropped_full: u64,

    pub rx_enqueued_frames: u64,
    pub rx_dropped_oversize: u64,
    pub rx_dropped_full: u64,
}

/// A "dumb" L2 backend that forwards raw Ethernet frames to/from an external transport.
///
/// This backend is intended to support Option C "L2 tunnel" networking where the emulator does
/// **not** run an in-guest NAT/network stack. Instead, frames are passed to/from the host (e.g.
/// browser JS) over a WebSocket/WebRTC tunnel.
///
/// - Guest → host frames are queued via [`NetworkBackend::transmit`] and can be retrieved using
///   [`L2TunnelBackend::drain_tx_frames`].
/// - Host → guest frames can be injected using [`L2TunnelBackend::push_rx_frame`] and are delivered
///   to the NIC models through [`NetworkBackend::poll_receive`].
///
/// Queues are bounded:
/// - Frames larger than `max_frame_bytes` are dropped.
/// - When a queue is full, new frames are dropped.
pub struct L2TunnelBackend {
    max_tx_frames: usize,
    max_rx_frames: usize,
    max_frame_bytes: usize,

    tx_frames: VecDeque<Vec<u8>>,
    rx_frames: VecDeque<Vec<u8>>,

    stats: L2TunnelBackendStats,
}

impl L2TunnelBackend {
    pub const DEFAULT_MAX_TX_FRAMES: usize = 1024;
    pub const DEFAULT_MAX_RX_FRAMES: usize = 1024;
    pub const DEFAULT_MAX_FRAME_BYTES: usize = 2048;

    /// Create a backend with default queue limits.
    pub fn new() -> Self {
        Self::with_limits(
            Self::DEFAULT_MAX_TX_FRAMES,
            Self::DEFAULT_MAX_RX_FRAMES,
            Self::DEFAULT_MAX_FRAME_BYTES,
        )
    }

    /// Create a backend with explicit queue limits.
    pub fn with_limits(max_tx_frames: usize, max_rx_frames: usize, max_frame_bytes: usize) -> Self {
        Self {
            max_tx_frames,
            max_rx_frames,
            max_frame_bytes,
            tx_frames: VecDeque::new(),
            rx_frames: VecDeque::new(),
            stats: L2TunnelBackendStats::default(),
        }
    }

    /// Drain all queued guest → host frames (FIFO order).
    pub fn drain_tx_frames(&mut self) -> Vec<Vec<u8>> {
        self.tx_frames.drain(..).collect()
    }

    /// Push a host → guest frame into the receive queue.
    ///
    /// Frames may be dropped if oversized or if the RX queue is full.
    pub fn push_rx_frame(&mut self, frame: Vec<u8>) {
        if frame.len() > self.max_frame_bytes {
            self.stats.rx_dropped_oversize += 1;
            return;
        }

        if self.rx_frames.len() >= self.max_rx_frames {
            self.stats.rx_dropped_full += 1;
            return;
        }

        self.stats.rx_enqueued_frames += 1;
        self.rx_frames.push_back(frame);
    }

    pub fn stats(&self) -> L2TunnelBackendStats {
        self.stats
    }
}

impl Default for L2TunnelBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for L2TunnelBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        if frame.len() > self.max_frame_bytes {
            self.stats.tx_dropped_oversize += 1;
            return;
        }

        if self.tx_frames.len() >= self.max_tx_frames {
            self.stats.tx_dropped_full += 1;
            return;
        }

        self.stats.tx_enqueued_frames += 1;
        self.tx_frames.push_back(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.rx_frames.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transmit_enqueues_and_drain_returns_fifo() {
        let mut backend = L2TunnelBackend::with_limits(8, 8, 2048);

        backend.transmit(vec![1, 2, 3]);
        backend.transmit(vec![4, 5]);

        assert_eq!(backend.drain_tx_frames(), vec![vec![1, 2, 3], vec![4, 5]]);
        assert_eq!(backend.drain_tx_frames(), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn rx_injection_and_poll_receive_fifo() {
        let mut backend = L2TunnelBackend::with_limits(8, 8, 2048);

        backend.push_rx_frame(vec![9]);
        backend.push_rx_frame(vec![8, 7]);

        assert_eq!(backend.poll_receive(), Some(vec![9]));
        assert_eq!(backend.poll_receive(), Some(vec![8, 7]));
        assert_eq!(backend.poll_receive(), None);
    }

    #[test]
    fn oversized_frames_are_dropped_and_counted() {
        let mut backend = L2TunnelBackend::with_limits(8, 8, 2);

        backend.transmit(vec![0, 1, 2]);
        backend.push_rx_frame(vec![3, 4, 5]);

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

        assert_eq!(backend.drain_tx_frames(), Vec::<Vec<u8>>::new());
        assert_eq!(backend.poll_receive(), None);
    }

    #[test]
    fn queue_full_drops_are_counted() {
        let mut backend = L2TunnelBackend::with_limits(1, 1, 2048);

        backend.transmit(vec![1]);
        backend.transmit(vec![2]);

        backend.push_rx_frame(vec![3]);
        backend.push_rx_frame(vec![4]);

        assert_eq!(
            backend.stats(),
            L2TunnelBackendStats {
                tx_enqueued_frames: 1,
                tx_dropped_oversize: 0,
                tx_dropped_full: 1,
                rx_enqueued_frames: 1,
                rx_dropped_oversize: 0,
                rx_dropped_full: 1,
            }
        );

        assert_eq!(backend.drain_tx_frames(), vec![vec![1]]);
        assert_eq!(backend.poll_receive(), Some(vec![3]));
        assert_eq!(backend.poll_receive(), None);
    }
}
