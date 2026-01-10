use crate::memory::GuestMemory;
use crate::queue::{DescriptorChain, VirtQueue};
use core::any::Any;

pub mod blk;
pub mod input;
pub mod net;
pub mod net_offload;
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
pub trait VirtioDevice: Any {
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

    /// Allow the device to perform work even when the guest did not kick the queue.
    ///
    /// This is primarily used for device → guest paths (e.g. network RX, input events)
    /// where the guest first posts buffers and the host later has data to place into
    /// those buffers.
    ///
    /// Return `Ok(true)` when the device produced used entries that should trigger an
    /// interrupt.
    fn poll_queue(
        &mut self,
        _queue_index: u16,
        _queue: &mut VirtQueue,
        _mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        Ok(false)
    }

    /// Read from device-specific configuration space.
    fn read_config(&self, offset: u64, data: &mut [u8]);

    /// Write to device-specific configuration space.
    fn write_config(&mut self, offset: u64, data: &[u8]);

    /// Reset to power-on state.
    fn reset(&mut self);

    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;
}
