use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};

pub const VIRTIO_DEVICE_TYPE_INPUT: u16 = 18;

#[derive(Debug, Clone, Copy)]
pub struct VirtioInputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: u32,
}

impl VirtioInputEvent {
    fn to_le_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..2].copy_from_slice(&self.type_.to_le_bytes());
        out[2..4].copy_from_slice(&self.code.to_le_bytes());
        out[4..8].copy_from_slice(&self.value.to_le_bytes());
        out
    }
}

pub struct VirtioInput {
    pending: std::collections::VecDeque<VirtioInputEvent>,
    buffers: std::collections::VecDeque<DescriptorChain>,
}

impl VirtioInput {
    pub fn new() -> Self {
        Self {
            pending: std::collections::VecDeque::new(),
            buffers: std::collections::VecDeque::new(),
        }
    }

    pub fn push_event(&mut self, event: VirtioInputEvent) {
        self.pending.push_back(event);
    }
}

impl Default for VirtioInput {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioDevice for VirtioInput {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_INPUT
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC
    }

    fn set_features(&mut self, _features: u64) {}

    fn num_queues(&self) -> u16 {
        // eventq + statusq.
        2
    }

    fn queue_max_size(&self, _queue: u16) -> u16 {
        64
    }

    fn process_queue(
        &mut self,
        queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        match queue_index {
            // eventq
            0 => {
                self.buffers.push_back(chain);
                self.flush_events(queue, mem)
            }
            // statusq
            1 => {
                // Contract v1: the guest may post LED/output events. We don't model LEDs
                // yet, but we must consume and complete the chain so the guest driver
                // can reclaim the descriptors.
                queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)
            }
            _ => Err(VirtioDeviceError::Unsupported),
        }
    }

    fn poll_queue(
        &mut self,
        queue_index: u16,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        if queue_index != 0 {
            return Ok(false);
        }
        self.flush_events(queue, mem)
    }

    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        // Full virtio-input config is quite involved; drivers typically probe it.
        // For now, expose an empty device and rely on a custom driver that only
        // consumes the event queue.
        data.fill(0);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn reset(&mut self) {
        self.pending.clear();
        self.buffers.clear();
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl VirtioInput {
    fn flush_events(
        &mut self,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let mut need_irq = false;
        while let Some(chain) = self.buffers.pop_front() {
            let Some(event) = self.pending.pop_front() else {
                self.buffers.push_front(chain);
                break;
            };

            let bytes = event.to_le_bytes();
            let descs = chain.descriptors();
            if descs.is_empty() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }

            let mut written = 0usize;
            for d in descs {
                if !d.is_write_only() {
                    return Err(VirtioDeviceError::BadDescriptorChain);
                }
                if written == bytes.len() {
                    break;
                }
                let take = (d.len as usize).min(bytes.len() - written);
                let dst = mem
                    .get_slice_mut(d.addr, take)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                dst.copy_from_slice(&bytes[written..written + take]);
                written += take;
            }

            need_irq |= queue
                .add_used(mem, chain.head_index(), written as u32)
                .map_err(|_| VirtioDeviceError::IoError)?;
        }

        Ok(need_irq)
    }
}
