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

    /// Poll for a host → guest Ethernet frame.
    ///
    /// NIC models like the E1000 can call this during their poll loop to allow a user-space
    /// network stack backend to return immediate responses (ARP/DHCP/DNS, etc.) in the same tick.
    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        None
    }
}
