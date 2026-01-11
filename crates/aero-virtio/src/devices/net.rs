use crate::devices::net_offload::{self, VirtioNetHdr};
use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use std::collections::VecDeque;

pub const VIRTIO_DEVICE_TYPE_NET: u16 = 1;

pub const VIRTIO_NET_F_CSUM: u64 = 1 << 0;
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
pub const VIRTIO_NET_F_HOST_TSO4: u64 = 1 << 11;
pub const VIRTIO_NET_F_HOST_TSO6: u64 = 1 << 12;
pub const VIRTIO_NET_F_HOST_ECN: u64 = 1 << 13;
pub const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;

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
            | VIRTIO_NET_F_MAC
            | VIRTIO_NET_F_MRG_RXBUF;

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
    fn negotiated_hdr_len(&self) -> usize {
        if (self.negotiated_features & VIRTIO_NET_F_MRG_RXBUF) != 0 {
            VirtioNetHdr::LEN
        } else {
            VirtioNetHdr::BASE_LEN
        }
    }

    fn process_tx(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let descs = chain.descriptors();
        if descs.is_empty() {
            return queue
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError);
        }

        let hdr_len = self.negotiated_hdr_len();
        let mut hdr_bytes = [0u8; VirtioNetHdr::LEN];
        let mut hdr_written = 0usize;
        let mut packet = Vec::new();
        let mut valid_chain = true;

        for d in descs {
            if d.is_write_only() {
                valid_chain = false;
                break;
            }
            let mut slice = mem
                .get_slice(d.addr, d.len as usize)
                .map_err(|_| VirtioDeviceError::IoError)?;

            if hdr_written < hdr_len {
                let take = (hdr_len - hdr_written).min(slice.len());
                hdr_bytes[hdr_written..hdr_written + take].copy_from_slice(&slice[..take]);
                hdr_written += take;
                slice = &slice[take..];
            }

            packet.extend_from_slice(slice);
        }

        valid_chain &= hdr_written == hdr_len;

        if valid_chain {
            if let Some(hdr) = VirtioNetHdr::from_slice_le(&hdr_bytes[..hdr_len]) {
                let wants_offload =
                    hdr.needs_csum() || hdr.gso_type_base() != net_offload::VIRTIO_NET_HDR_GSO_NONE;

                let mut allow_offload = true;
                if wants_offload && self.offload_config.disable_offloads {
                    allow_offload = false;
                }
                if hdr.needs_csum() && (self.negotiated_features & VIRTIO_NET_F_CSUM) == 0 {
                    allow_offload = false;
                }
                match hdr.gso_type_base() {
                    net_offload::VIRTIO_NET_HDR_GSO_NONE => {}
                    net_offload::VIRTIO_NET_HDR_GSO_TCPV4 => {
                        if (self.negotiated_features & VIRTIO_NET_F_HOST_TSO4) == 0 {
                            allow_offload = false;
                        }
                    }
                    net_offload::VIRTIO_NET_HDR_GSO_TCPV6 => {
                        if (self.negotiated_features & VIRTIO_NET_F_HOST_TSO6) == 0 {
                            allow_offload = false;
                        }
                    }
                    _ => {
                        allow_offload = false;
                    }
                }
                if hdr.has_ecn() && (self.negotiated_features & VIRTIO_NET_F_HOST_ECN) == 0 {
                    allow_offload = false;
                }

                if wants_offload && allow_offload {
                    if let Ok(tx_packets) = net_offload::process_tx_packet(hdr, &packet) {
                        for pkt in tx_packets {
                            self.backend.transmit(&pkt);
                        }
                    }
                } else if !wants_offload {
                    self.backend.transmit(&packet);
                }
            }
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
        let hdr_len = self.negotiated_hdr_len();
        let header_bytes = VirtioNetHdr {
            flags: 0,
            gso_type: 0,
            hdr_len: 0,
            gso_size: 0,
            csum_start: 0,
            csum_offset: 0,
            num_buffers: if hdr_len == VirtioNetHdr::LEN { 1 } else { 0 },
        }
        .to_bytes_le();

        while let Some(chain) = self.rx_buffers.pop_front() {
            let descs = chain.descriptors();
            if descs.is_empty() || descs.iter().any(|d| !d.is_write_only()) {
                need_irq |= queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                continue;
            }

            let Some(pkt) = self.backend.poll_receive() else {
                self.rx_buffers.push_front(chain);
                break;
            };

            let mut written = 0usize;
            let mut header_off = 0usize;
            let mut remaining_pkt = pkt.as_slice();

            for d in descs {
                let dst = mem
                    .get_slice_mut(d.addr, d.len as usize)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                let mut off = 0usize;
                if header_off < hdr_len {
                    let take = (hdr_len - header_off).min(dst.len());
                    dst[..take].copy_from_slice(&header_bytes[header_off..header_off + take]);
                    header_off += take;
                    off += take;
                    written += take;
                }
                if off < dst.len() && !remaining_pkt.is_empty() {
                    let take = remaining_pkt.len().min(dst.len() - off);
                    dst[off..off + take].copy_from_slice(&remaining_pkt[..take]);
                    remaining_pkt = &remaining_pkt[take..];
                    written += take;
                }
                if header_off == hdr_len && remaining_pkt.is_empty() {
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
    use crate::memory::{read_u16_le, write_u16_le, write_u32_le, write_u64_le, GuestRam};
    use crate::queue::{VirtQueueConfig, VIRTQ_DESC_F_NEXT};

    fn write_desc(
        mem: &mut GuestRam,
        table: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = table + u64::from(index) * 16;
        write_u64_le(mem, base, addr).unwrap();
        write_u32_le(mem, base + 8, len).unwrap();
        write_u16_le(mem, base + 12, flags).unwrap();
        write_u16_le(mem, base + 14, next).unwrap();
    }

    fn build_ipv4_tcp_packet(payload_len: usize, flags: u8) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]); // dst
        packet.extend_from_slice(&[0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb]); // src
        packet.extend_from_slice(&0x0800u16.to_be_bytes());

        let total_len = (20 + 20 + payload_len) as u16;
        let identification = 0x1111u16;
        let mut ipv4 = [0u8; 20];
        ipv4[0] = (4 << 4) | 5;
        ipv4[2..4].copy_from_slice(&total_len.to_be_bytes());
        ipv4[4..6].copy_from_slice(&identification.to_be_bytes());
        ipv4[8] = 64;
        ipv4[9] = 6;
        ipv4[12..16].copy_from_slice(&[10, 0, 0, 1]);
        ipv4[16..20].copy_from_slice(&[10, 0, 0, 2]);
        ipv4[10..12].copy_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&ipv4);

        let seq = 0x01020304u32;
        let ack = 0u32;
        let mut tcp = [0u8; 20];
        tcp[0..2].copy_from_slice(&1000u16.to_be_bytes());
        tcp[2..4].copy_from_slice(&2000u16.to_be_bytes());
        tcp[4..8].copy_from_slice(&seq.to_be_bytes());
        tcp[8..12].copy_from_slice(&ack.to_be_bytes());
        tcp[12] = 5u8 << 4;
        tcp[13] = flags;
        tcp[14..16].copy_from_slice(&4096u16.to_be_bytes());
        packet.extend_from_slice(&tcp);

        packet.extend(std::iter::repeat(0x42u8).take(payload_len));
        packet
    }

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

    #[test]
    fn tx_offload_requested_but_not_negotiated_does_not_stall_queue() {
        let mut dev = VirtioNet::new(LoopbackNet::default(), [0; 6]);
        dev.set_features(0);

        let mut mem = GuestRam::new(0x10000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x4000;
        let packet_addr = 0x5000;

        let hdr_len = (14 + 20 + 20) as u16;
        let mss = 1000u16;
        let mut hdr_bytes = [0u8; VirtioNetHdr::BASE_LEN];
        hdr_bytes[0] = net_offload::VIRTIO_NET_HDR_F_NEEDS_CSUM;
        hdr_bytes[1] = net_offload::VIRTIO_NET_HDR_GSO_TCPV4;
        hdr_bytes[2..4].copy_from_slice(&hdr_len.to_le_bytes());
        hdr_bytes[4..6].copy_from_slice(&mss.to_le_bytes());
        mem.write(header_addr, &hdr_bytes).unwrap();

        let packet = build_ipv4_tcp_packet(3000, 0x18);
        mem.write(packet_addr, &packet).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            VirtioNetHdr::BASE_LEN as u32,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            packet_addr,
            packet.len() as u32,
            0,
            0,
        );

        // avail idx = 1, ring[0] = 0
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();

        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: 8,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let chain = queue.pop_descriptor_chain(&mem).unwrap().unwrap();
        let irq = dev.process_queue(1, chain, &mut queue, &mut mem).unwrap();
        assert!(irq);
        assert!(dev.backend_mut().tx_packets.is_empty());

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    }

    #[test]
    fn tx_gso_with_negotiated_features_segments_and_transmits() {
        let mut dev = VirtioNet::new(LoopbackNet::default(), [0; 6]);
        dev.set_features(VIRTIO_NET_F_CSUM | VIRTIO_NET_F_HOST_TSO4);

        let mut mem = GuestRam::new(0x10000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x4000;
        let packet_addr = 0x5000;

        let hdr_len = (14 + 20 + 20) as u16;
        let mss = 1000u16;
        let mut hdr_bytes = [0u8; VirtioNetHdr::BASE_LEN];
        hdr_bytes[0] = net_offload::VIRTIO_NET_HDR_F_NEEDS_CSUM;
        hdr_bytes[1] = net_offload::VIRTIO_NET_HDR_GSO_TCPV4;
        hdr_bytes[2..4].copy_from_slice(&hdr_len.to_le_bytes());
        hdr_bytes[4..6].copy_from_slice(&mss.to_le_bytes());
        mem.write(header_addr, &hdr_bytes).unwrap();

        let packet = build_ipv4_tcp_packet(3000, 0x18);
        mem.write(packet_addr, &packet).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            VirtioNetHdr::BASE_LEN as u32,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            packet_addr,
            packet.len() as u32,
            0,
            0,
        );

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();

        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: 8,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let chain = queue.pop_descriptor_chain(&mem).unwrap().unwrap();
        dev.process_queue(1, chain, &mut queue, &mut mem).unwrap();

        assert_eq!(dev.backend_mut().tx_packets.len(), 3);
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    }
}
