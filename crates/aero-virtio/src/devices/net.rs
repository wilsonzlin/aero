use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::pci::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use crate::memory::GuestMemory;

pub const VIRTIO_DEVICE_TYPE_NET: u16 = 1;

pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;

const VIRTIO_NET_HDR_LEN: usize = 10;

pub trait NetBackend {
    fn transmit(&mut self, packet: &[u8]);
    fn poll_receive(&mut self) -> Option<Vec<u8>>;
}

#[derive(Default, Debug)]
pub struct LoopbackNet {
    pub tx_packets: Vec<Vec<u8>>,
    pub rx_packets: Vec<Vec<u8>>,
}

impl NetBackend for LoopbackNet {
    fn transmit(&mut self, packet: &[u8]) {
        self.tx_packets.push(packet.to_vec());
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        if self.rx_packets.is_empty() {
            None
        } else {
            Some(self.rx_packets.remove(0))
        }
    }
}

pub struct VirtioNet<B: NetBackend> {
    backend: B,
    mac: [u8; 6],
}

impl<B: NetBackend> VirtioNet<B> {
    pub fn new(backend: B, mac: [u8; 6]) -> Self {
        Self { backend, mac }
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
}

impl<B: NetBackend + 'static> VirtioDevice for VirtioNet<B> {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_NET
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_F_RING_EVENT_IDX | VIRTIO_NET_F_MAC
    }

    fn set_features(&mut self, _features: u64) {}

    fn num_queues(&self) -> u16 {
        2
    }

    fn queue_max_size(&self, _queue: u16) -> u16 {
        256
    }

    fn process_queue(
        &mut self,
        queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        match queue_index {
            0 => self.process_rx(chain, queue, mem),
            1 => self.process_tx(chain, queue, mem),
            _ => Err(VirtioDeviceError::Unsupported),
        }
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        data.fill(0);
        let start = offset as usize;
        if start < self.mac.len() {
            let end = (start + data.len()).min(self.mac.len());
            data[..end - start].copy_from_slice(&self.mac[start..end]);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn reset(&mut self) {}
}

impl<B: NetBackend> VirtioNet<B> {
    fn process_tx(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let descs = chain.descriptors();
        if descs.is_empty() {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        // Build packet (skip the virtio_net_hdr).
        let mut skip = VIRTIO_NET_HDR_LEN;
        let mut packet = Vec::new();
        for d in descs {
            if d.is_write_only() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }
            let mut slice = mem
                .get_slice(d.addr, d.len as usize)
                .map_err(|_| VirtioDeviceError::IoError)?;
            if skip != 0 {
                if slice.len() <= skip {
                    skip -= slice.len();
                    continue;
                }
                slice = &slice[skip..];
                skip = 0;
            }
            packet.extend_from_slice(slice);
        }
        self.backend.transmit(&packet);

        queue
            .add_used(mem, chain.head_index(), 0)
            .map_err(|_| VirtioDeviceError::IoError)
    }

    fn process_rx(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let Some(pkt) = self.backend.poll_receive() else {
            // No packet available; keep the buffer until later. In a real device we'd
            // stash it, but for now just return it unused.
            return queue
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError);
        };

        let descs = chain.descriptors();
        if descs.is_empty() {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }
        for d in descs {
            if !d.is_write_only() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }
        }

        let mut written = 0usize;
        let mut remaining_header = VIRTIO_NET_HDR_LEN;
        let mut remaining_pkt = pkt.as_slice();

        for d in descs {
            let dst = mem
                .get_slice_mut(d.addr, d.len as usize)
                .map_err(|_| VirtioDeviceError::IoError)?;
            let mut off = 0usize;
            if remaining_header != 0 {
                let take = remaining_header.min(dst.len());
                dst[..take].fill(0);
                remaining_header -= take;
                off += take;
                written += take;
            }
            if off < dst.len() && !remaining_pkt.is_empty() {
                let take = remaining_pkt.len().min(dst.len() - off);
                dst[off..off + take].copy_from_slice(&remaining_pkt[..take]);
                remaining_pkt = &remaining_pkt[take..];
                written += take;
            }
            if remaining_header == 0 && remaining_pkt.is_empty() {
                break;
            }
        }

        queue
            .add_used(mem, chain.head_index(), written as u32)
            .map_err(|_| VirtioDeviceError::IoError)
    }
}
