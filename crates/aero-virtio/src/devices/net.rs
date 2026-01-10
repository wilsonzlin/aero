use crate::devices::net_offload::{self, VirtioNetHdr};
use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use std::collections::VecDeque;

pub const VIRTIO_DEVICE_TYPE_NET: u16 = 1;

pub const VIRTIO_NET_F_CSUM: u64 = 1 << 0;
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
pub const VIRTIO_NET_F_HOST_TSO4: u64 = 1 << 11;
pub const VIRTIO_NET_F_HOST_TSO6: u64 = 1 << 12;
pub const VIRTIO_NET_F_HOST_ECN: u64 = 1 << 13;

const VIRTIO_NET_HDR_LEN: usize = VirtioNetHdr::LEN;

#[derive(Debug, Clone, Copy, Default)]
pub struct VirtioNetOffloadConfig {
    pub disable_offloads: bool,
}

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
    offload_config: VirtioNetOffloadConfig,
    negotiated_features: u64,
    rx_buffers: VecDeque<DescriptorChain>,
}

impl<B: NetBackend> VirtioNet<B> {
    pub fn new(backend: B, mac: [u8; 6]) -> Self {
        Self::new_with_offload_config(backend, mac, VirtioNetOffloadConfig::default())
    }

    pub fn new_with_offload_config(
        backend: B,
        mac: [u8; 6],
        offload_config: VirtioNetOffloadConfig,
    ) -> Self {
        Self {
            backend,
            mac,
            offload_config,
            negotiated_features: 0,
            rx_buffers: VecDeque::new(),
        }
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
        let mut features = VIRTIO_F_VERSION_1
            | VIRTIO_F_RING_INDIRECT_DESC
            | VIRTIO_F_RING_EVENT_IDX
            | VIRTIO_NET_F_MAC;

        if !self.offload_config.disable_offloads {
            features |= VIRTIO_NET_F_CSUM
                | VIRTIO_NET_F_HOST_TSO4
                | VIRTIO_NET_F_HOST_TSO6
                | VIRTIO_NET_F_HOST_ECN;
        }

        features
    }

    fn set_features(&mut self, features: u64) {
        self.negotiated_features = features;
    }

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

    fn poll_queue(
        &mut self,
        queue_index: u16,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        if queue_index != 0 {
            return Ok(false);
        }
        self.flush_rx(queue, mem)
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

    fn reset(&mut self) {
        self.negotiated_features = 0;
        self.rx_buffers.clear();
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
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

        let mut hdr_bytes = [0u8; VIRTIO_NET_HDR_LEN];
        let mut hdr_written = 0usize;
        let mut packet = Vec::new();

        for d in descs {
            if d.is_write_only() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }
            let mut slice = mem
                .get_slice(d.addr, d.len as usize)
                .map_err(|_| VirtioDeviceError::IoError)?;

            if hdr_written < VIRTIO_NET_HDR_LEN {
                let take = (VIRTIO_NET_HDR_LEN - hdr_written).min(slice.len());
                hdr_bytes[hdr_written..hdr_written + take].copy_from_slice(&slice[..take]);
                hdr_written += take;
                slice = &slice[take..];
            }

            packet.extend_from_slice(slice);
        }

        if hdr_written != VIRTIO_NET_HDR_LEN {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        let hdr = VirtioNetHdr::from_bytes_le(hdr_bytes);
        if self.offload_config.disable_offloads
            && (hdr.needs_csum() || hdr.gso_type_base() != net_offload::VIRTIO_NET_HDR_GSO_NONE)
        {
            return Err(VirtioDeviceError::Unsupported);
        }

        if hdr.needs_csum() && (self.negotiated_features & VIRTIO_NET_F_CSUM) == 0 {
            return Err(VirtioDeviceError::Unsupported);
        }

        match hdr.gso_type_base() {
            net_offload::VIRTIO_NET_HDR_GSO_NONE => {}
            net_offload::VIRTIO_NET_HDR_GSO_TCPV4 => {
                if (self.negotiated_features & VIRTIO_NET_F_HOST_TSO4) == 0 {
                    return Err(VirtioDeviceError::Unsupported);
                }
            }
            net_offload::VIRTIO_NET_HDR_GSO_TCPV6 => {
                if (self.negotiated_features & VIRTIO_NET_F_HOST_TSO6) == 0 {
                    return Err(VirtioDeviceError::Unsupported);
                }
            }
            _ => return Err(VirtioDeviceError::Unsupported),
        }

        if hdr.has_ecn() && (self.negotiated_features & VIRTIO_NET_F_HOST_ECN) == 0 {
            return Err(VirtioDeviceError::Unsupported);
        }

        let tx_packets = net_offload::process_tx_packet(hdr, &packet)
            .map_err(|_| VirtioDeviceError::Unsupported)?;

        for pkt in tx_packets {
            self.backend.transmit(&pkt);
        }

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
        self.rx_buffers.push_back(chain);
        self.flush_rx(queue, mem)
    }

    fn flush_rx(
        &mut self,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let mut need_irq = false;

        while let Some(chain) = self.rx_buffers.pop_front() {
            let Some(pkt) = self.backend.poll_receive() else {
                self.rx_buffers.push_front(chain);
                break;
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

            need_irq |= queue
                .add_used(mem, chain.head_index(), written as u32)
                .map_err(|_| VirtioDeviceError::IoError)?;
        }

        Ok(need_irq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_negotiation_hides_offloads_when_disabled() {
        let dev = VirtioNet::new_with_offload_config(
            LoopbackNet::default(),
            [0; 6],
            VirtioNetOffloadConfig {
                disable_offloads: true,
            },
        );
        let features = <VirtioNet<LoopbackNet> as VirtioDevice>::device_features(&dev);
        assert_eq!(features & VIRTIO_NET_F_CSUM, 0);
        assert_eq!(features & VIRTIO_NET_F_HOST_TSO4, 0);
        assert_eq!(features & VIRTIO_NET_F_HOST_TSO6, 0);
    }

    #[test]
    fn feature_negotiation_advertises_offloads_when_enabled() {
        let dev = VirtioNet::new_with_offload_config(
            LoopbackNet::default(),
            [0; 6],
            VirtioNetOffloadConfig {
                disable_offloads: false,
            },
        );
        let features = <VirtioNet<LoopbackNet> as VirtioDevice>::device_features(&dev);
        assert_ne!(features & VIRTIO_NET_F_CSUM, 0);
        assert_ne!(features & VIRTIO_NET_F_HOST_TSO4, 0);
        assert_ne!(features & VIRTIO_NET_F_HOST_TSO6, 0);
    }
}
