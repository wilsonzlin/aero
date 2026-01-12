#[cfg(feature = "legacy-audio")]
pub mod audio;
pub mod input;
pub mod net;
pub mod pci;
pub mod serial;
pub mod storage;
pub mod usb;
pub mod virtio;

/// Trait implemented by devices that respond to x86 IN/OUT instructions.
///
/// `size` is the access size in bytes (1/2/4). Implementations should handle
/// any size gracefully, even if a port is traditionally byte-wide.
pub trait PortIO {
    fn port_read(&self, port: u16, size: usize) -> u32;
    fn port_write(&mut self, port: u16, size: usize, val: u32);
}
