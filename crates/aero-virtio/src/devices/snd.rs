use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::pci::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use crate::memory::GuestMemory;

pub const VIRTIO_DEVICE_TYPE_SND: u16 = 25;

/// Minimal virtio-snd device model.
///
/// The full virtio-snd spec is quite large (control messages, PCM streams,
/// events, jack handling, etc). For now this model only implements queue wiring
/// and a best-effort completion path so a custom guest driver can be developed
/// incrementally.
pub struct VirtioSnd {
    features: u64,
}

impl VirtioSnd {
    pub fn new() -> Self {
        Self { features: 0 }
    }
}

impl Default for VirtioSnd {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioDevice for VirtioSnd {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_SND
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_F_RING_EVENT_IDX
    }

    fn set_features(&mut self, features: u64) {
        self.features = features;
    }

    fn num_queues(&self) -> u16 {
        // controlq + eventq + txq + rxq
        4
    }

    fn queue_max_size(&self, queue: u16) -> u16 {
        match queue {
            0 | 1 => 64,
            _ => 256,
        }
    }

    fn process_queue(
        &mut self,
        _queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let _ = self.features;

        // Best-effort: write a 32-bit "unsupported" status into the first
        // writable descriptor if present.
        let mut written = 0u32;
        for d in chain.descriptors() {
            if !d.is_write_only() || d.len == 0 {
                continue;
            }
            let dst = mem
                .get_slice_mut(d.addr, (d.len as usize).min(4))
                .map_err(|_| VirtioDeviceError::IoError)?;
            dst.fill(0xff);
            written = dst.len() as u32;
            break;
        }

        queue
            .add_used(mem, chain.head_index(), written)
            .map_err(|_| VirtioDeviceError::IoError)
    }

    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        // virtio-snd config begins with the number of jacks/streams/channel maps.
        // Leave these as 0 for now.
        data.fill(0);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn reset(&mut self) {
        self.features = 0;
    }
}

