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

    /// Best-effort stats for the ring-buffer-backed [`L2TunnelRingBackend`] (`NET_TX`/`NET_RX`
    /// rings).
    ///
    /// Most backends do not expose ring stats (and return `None`). This is primarily used for
    /// debugging/instrumentation in host glue.
    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        None
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for Box<T> {
    fn transmit(&mut self, frame: Vec<u8>) {
        <T as NetworkBackend>::transmit(&mut **self, frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        <T as NetworkBackend>::poll_receive(&mut **self)
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        <T as NetworkBackend>::l2_ring_stats(&**self)
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for &mut T {
    fn transmit(&mut self, frame: Vec<u8>) {
        <T as NetworkBackend>::transmit(&mut **self, frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        <T as NetworkBackend>::poll_receive(&mut **self)
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        <T as NetworkBackend>::l2_ring_stats(&**self)
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

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.as_mut().and_then(|backend| backend.poll_receive())
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.as_ref().and_then(|backend| backend.l2_ring_stats())
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for std::cell::RefCell<T> {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.get_mut().transmit(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.get_mut().poll_receive()
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.borrow().l2_ring_stats()
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for std::rc::Rc<std::cell::RefCell<T>> {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.borrow_mut().transmit(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.borrow_mut().poll_receive()
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.borrow().l2_ring_stats()
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for std::sync::Mutex<T> {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .transmit(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .poll_receive()
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .l2_ring_stats()
    }
}

impl<T: NetworkBackend + ?Sized> NetworkBackend for std::sync::Arc<std::sync::Mutex<T>> {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .transmit(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .poll_receive()
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .l2_ring_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::{L2TunnelRingBackendStats, NetworkBackend};

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    #[test]
    fn network_backend_is_implemented_for_rc_refcell() {
        #[derive(Default)]
        struct Backend {
            tx: Vec<Vec<u8>>,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetworkBackend for Backend {
            fn transmit(&mut self, frame: Vec<u8>) {
                self.tx.push(frame);
            }

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.rx.pop_front()
            }

            fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
                Some(L2TunnelRingBackendStats {
                    tx_pushed_frames: 1,
                    ..Default::default()
                })
            }
        }

        let inner = Rc::new(RefCell::new(Backend::default()));
        inner.borrow_mut().rx.push_back(vec![9, 9, 9]);

        let mut backend = inner.clone();
        backend.transmit(vec![1, 2, 3]);

        assert_eq!(
            backend.l2_ring_stats(),
            Some(L2TunnelRingBackendStats {
                tx_pushed_frames: 1,
                ..Default::default()
            })
        );
        assert_eq!(backend.poll_receive(), Some(vec![9, 9, 9]));
        assert_eq!(backend.poll_receive(), None);

        assert_eq!(inner.borrow().tx, vec![vec![1, 2, 3]]);
    }

    #[test]
    fn network_backend_is_implemented_for_refcell() {
        #[derive(Default)]
        struct Backend {
            tx: Vec<Vec<u8>>,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetworkBackend for Backend {
            fn transmit(&mut self, frame: Vec<u8>) {
                self.tx.push(frame);
            }

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.rx.pop_front()
            }

            fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
                Some(L2TunnelRingBackendStats {
                    tx_pushed_frames: 1,
                    ..Default::default()
                })
            }
        }

        let mut backend = RefCell::new(Backend::default());
        backend.get_mut().rx.push_back(vec![9, 9, 9]);

        backend.transmit(vec![1, 2, 3]);

        assert_eq!(
            backend.l2_ring_stats(),
            Some(L2TunnelRingBackendStats {
                tx_pushed_frames: 1,
                ..Default::default()
            })
        );
        assert_eq!(backend.poll_receive(), Some(vec![9, 9, 9]));
        assert_eq!(backend.poll_receive(), None);

        assert_eq!(backend.borrow().tx, vec![vec![1, 2, 3]]);
    }

    #[test]
    fn network_backend_is_implemented_for_arc_mutex() {
        #[derive(Default)]
        struct Backend {
            tx: Vec<Vec<u8>>,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetworkBackend for Backend {
            fn transmit(&mut self, frame: Vec<u8>) {
                self.tx.push(frame);
            }

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.rx.pop_front()
            }

            fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
                Some(L2TunnelRingBackendStats {
                    rx_popped_frames: 1,
                    ..Default::default()
                })
            }
        }

        let inner = Arc::new(Mutex::new(Backend::default()));
        inner.lock().unwrap().rx.push_back(vec![9, 9, 9]);

        let mut backend = inner.clone();
        backend.transmit(vec![1, 2, 3]);

        assert_eq!(
            backend.l2_ring_stats(),
            Some(L2TunnelRingBackendStats {
                rx_popped_frames: 1,
                ..Default::default()
            })
        );
        assert_eq!(backend.poll_receive(), Some(vec![9, 9, 9]));
        assert_eq!(backend.poll_receive(), None);

        assert_eq!(inner.lock().unwrap().tx, vec![vec![1, 2, 3]]);
    }

    #[test]
    fn network_backend_is_implemented_for_mutex() {
        #[derive(Default)]
        struct Backend {
            tx: Vec<Vec<u8>>,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetworkBackend for Backend {
            fn transmit(&mut self, frame: Vec<u8>) {
                self.tx.push(frame);
            }

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.rx.pop_front()
            }

            fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
                Some(L2TunnelRingBackendStats {
                    tx_pushed_frames: 1,
                    ..Default::default()
                })
            }
        }

        let mut backend = Mutex::new(Backend::default());
        backend.lock().unwrap().rx.push_back(vec![9, 9, 9]);

        backend.transmit(vec![1, 2, 3]);

        assert_eq!(
            backend.l2_ring_stats(),
            Some(L2TunnelRingBackendStats {
                tx_pushed_frames: 1,
                ..Default::default()
            })
        );
        assert_eq!(backend.poll_receive(), Some(vec![9, 9, 9]));
        assert_eq!(backend.poll_receive(), None);

        assert_eq!(backend.lock().unwrap().tx, vec![vec![1, 2, 3]]);
    }
}
