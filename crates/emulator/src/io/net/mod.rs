pub mod e1000;
pub mod e1000_aero;
pub mod stack;
pub mod trace;

/// Network backend to bridge frames between emulated NICs and the host network stack.
///
/// This is intentionally minimal: devices only need a way to transmit Ethernet frames to the
/// outside world. Incoming frames are delivered via device-specific queues (e.g. RX rings).
pub trait NetworkBackend {
    fn transmit(&mut self, frame: Vec<u8>);
}
