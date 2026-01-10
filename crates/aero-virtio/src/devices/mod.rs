use crate::memory::GuestMemory;
use crate::queue::{DescriptorChain, VirtQueue};

pub mod blk;
pub mod input;
pub mod net;
pub mod snd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioDeviceError {
    BadDescriptorChain,
    Unsupported,
    IoError,
}

/// A virtio device model.
///
/// This trait is focused on the "device logic" side. The virtio-pci transport
/// (`crate::pci`) handles feature negotiation, queue configuration, and
/// interrupts.
pub trait VirtioDevice {
    /// Virtio device type (e.g. 1 = net, 2 = block).
    fn device_type(&self) -> u16;

    /// Total set of device features offered to the guest driver.
    fn device_features(&self) -> u64;

    /// Called after feature negotiation is complete.
    fn set_features(&mut self, features: u64);

    /// Number of virtqueues this device exposes.
    fn num_queues(&self) -> u16;

    /// Maximum queue size for a given virtqueue.
    fn queue_max_size(&self, queue: u16) -> u16;

    /// Handle a descriptor chain popped from `queue`.
    ///
    /// Returns `Ok(true)` if an interrupt should be delivered to the guest as a
    /// result of completing this chain.
    fn process_queue(
        &mut self,
        queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError>;

    /// Read from device-specific configuration space.
    fn read_config(&self, offset: u64, data: &mut [u8]);

    /// Write to device-specific configuration space.
    fn write_config(&mut self, offset: u64, data: &[u8]);

    /// Reset to power-on state.
    fn reset(&mut self);
}

