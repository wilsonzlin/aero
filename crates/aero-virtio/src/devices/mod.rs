use crate::memory::GuestMemory;
use crate::queue::{DescriptorChain, VirtQueue};
use core::any::Any;

pub mod blk;
pub mod gpu;
pub mod input;
pub mod net;
pub mod net_offload;
#[cfg(feature = "snd")]
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

    /// PCI subsystem device ID (`config[0x2e..0x30]`) for this device.
    ///
    /// Aero uses subsystem IDs as stable secondary identifiers, primarily for
    /// debugging and optional Windows INF matching.
    ///
    /// By default this mirrors the virtio device type (e.g. 0x0002 for virtio-blk,
    /// 0x0019 for virtio-snd), but some contracts may override it to distinguish
    /// device variants (e.g. virtio-input keyboard vs mouse).
    fn subsystem_device_id(&self) -> u16 {
        self.device_type()
    }

    /// PCI header type (`config[0x0e]`) for this device function.
    ///
    /// Most virtio-pci devices use a standard type-0 header (`0x00`). Devices that are exposed as
    /// part of a multi-function PCI device should set bit 7 (`0x80`) on function 0.
    fn pci_header_type(&self) -> u8 {
        0x00
    }

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

    /// Optional device-specific snapshot payload.
    ///
    /// The virtio-pci transport (`crate::pci`) snapshots negotiated features, virtqueue state, and
    /// interrupt latches. Some virtio devices also maintain additional state that is not described
    /// by the transport (for example, virtio-input keyboard LED state as last set by the guest via
    /// `statusq`).
    ///
    /// When provided, this byte blob is stored inside the virtio-pci snapshot and passed back to
    /// [`VirtioDevice::restore_device_state`] on restore.
    fn snapshot_device_state(&self) -> Option<Vec<u8>> {
        None
    }

    /// Restore a device-specific snapshot payload produced by [`VirtioDevice::snapshot_device_state`].
    ///
    /// Implementations should treat `bytes` as untrusted (it may come from a corrupted/malicious
    /// snapshot). Best-effort restore is preferred: invalid payloads should not panic or allocate
    /// unbounded memory.
    fn restore_device_state(&mut self, _bytes: &[u8]) {}

    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;
}
