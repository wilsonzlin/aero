use crate::devices::net_offload::{self, VirtioNetHdr};
use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
pub use aero_net_backend::NetworkBackend as NetBackend;
use std::collections::VecDeque;

pub const VIRTIO_DEVICE_TYPE_NET: u16 = 1;

pub const VIRTIO_NET_F_CSUM: u64 = 1 << 0;
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
pub const VIRTIO_NET_F_HOST_TSO4: u64 = 1 << 11;
pub const VIRTIO_NET_F_HOST_TSO6: u64 = 1 << 12;
pub const VIRTIO_NET_F_HOST_ECN: u64 = 1 << 13;
pub const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;

pub const VIRTIO_NET_S_LINK_UP: u16 = 1;

/// Upper bound on the amount of backend RX work performed per `flush_rx()` call.
///
/// This ensures `VirtioPciDevice::poll()` remains bounded even if a backend
/// misbehaves and always reports a packet ready.
const MAX_RX_FRAMES_PER_FLUSH: usize = 256;

#[derive(Default, Debug)]
pub struct LoopbackNet {
    pub tx_packets: Vec<Vec<u8>>,
    pub rx_packets: Vec<Vec<u8>>,
}

impl NetBackend for LoopbackNet {
    fn transmit(&mut self, packet: Vec<u8>) {
        self.tx_packets.push(packet);
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
    negotiated_features: u64,
    rx_buffers: VecDeque<DescriptorChain>,
}

impl<B: NetBackend> VirtioNet<B> {
    pub fn new(backend: B, mac: [u8; 6]) -> Self {
        Self {
            backend,
            mac,
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
        // Contract v1 (AERO-W7-VIRTIO) for virtio-net is intentionally strict:
        // - No mergeable RX buffers (fixed 10-byte virtio_net_hdr).
        // - No checksum/GSO offloads.
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS
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
        let mut cfg = [0u8; 10];
        cfg[0..6].copy_from_slice(&self.mac);
        cfg[6..8].copy_from_slice(&VIRTIO_NET_S_LINK_UP.to_le_bytes());
        cfg[8..10].copy_from_slice(&1u16.to_le_bytes()); // max_virtqueue_pairs

        let start = offset as usize;
        if start >= cfg.len() {
            data.fill(0);
            return;
        }
        let end = usize::min(cfg.len(), start + data.len());
        data[..end - start].copy_from_slice(&cfg[start..end]);
        if end - start < data.len() {
            data[end - start..].fill(0);
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
        let offload_features = VIRTIO_NET_F_CSUM
            | VIRTIO_NET_F_HOST_TSO4
            | VIRTIO_NET_F_HOST_TSO6
            | VIRTIO_NET_F_HOST_ECN;
        let max_packet_bytes = if (self.negotiated_features & offload_features) != 0 {
            1024 * 1024
        } else {
            // Contract v1 frames are at most 1514 bytes; keep one extra byte so we can detect
            // oversize frames without copying unbounded guest data.
            1515
        };
        let mut packet = Vec::with_capacity(max_packet_bytes.min(4096));
        let mut packet_truncated = false;
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

            if !slice.is_empty() {
                let remaining = max_packet_bytes.saturating_sub(packet.len());
                if remaining == 0 {
                    packet_truncated = true;
                } else {
                    let take = remaining.min(slice.len());
                    packet.extend_from_slice(&slice[..take]);
                    if take < slice.len() {
                        packet_truncated = true;
                    }
                }
            }
        }

        valid_chain &= hdr_written == hdr_len;
        valid_chain &= !packet_truncated;

        if valid_chain {
            if let Some(hdr) = VirtioNetHdr::from_slice_le(&hdr_bytes[..hdr_len]) {
                let wants_offload =
                    hdr.needs_csum() || hdr.gso_type_base() != net_offload::VIRTIO_NET_HDR_GSO_NONE;

                let mut allow_offload = true;
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

                // Contract v1 ignores virtio_net_hdr contents because no offload or mergeable
                // features are negotiated. For robustness, if a guest sets offload flags anyway,
                // we still transmit the packet unchanged (rather than silently dropping it).
                if wants_offload && allow_offload {
                    if let Ok(tx_packets) = net_offload::process_tx_packet(hdr, &packet) {
                        for pkt in tx_packets {
                            if pkt.len() >= 14 && pkt.len() <= 1514 {
                                self.backend.transmit(pkt);
                            }
                        }
                    }
                } else if packet.len() >= 14 && packet.len() <= 1514 {
                    self.backend.transmit(packet);
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
        // Prevent unbounded growth if a corrupted/malicious driver repeatedly publishes RX buffers
        // (e.g. by moving `avail.idx` far ahead and causing the device to re-consume stale ring
        // entries). A correct driver cannot have more outstanding RX buffers than the queue size.
        let max_rx_buffers = queue.size() as usize;
        if max_rx_buffers != 0 && self.rx_buffers.len() >= max_rx_buffers {
            let mut need_irq = queue
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError)?;
            need_irq |= self.flush_rx(queue, mem)?;
            return Ok(need_irq);
        }

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
        let header_bytes = VirtioNetHdr::default().to_bytes_le();

        // Drain any invalid RX buffers so they don't permanently block the queue.
        while let Some(chain) = self.rx_buffers.front() {
            let descs = chain.descriptors();
            if !descs.is_empty()
                && descs.iter().all(|d| d.is_write_only())
                && descs[0].len as usize >= hdr_len
            {
                break;
            }
            let chain = self.rx_buffers.pop_front().unwrap();
            need_irq |= queue
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError)?;
        }

        // If the guest has not posted any RX buffers yet, do not poll the backend. This avoids
        // dropping frames early (and prevents unbounded backend draining work) until the driver is
        // ready to receive.
        if self.rx_buffers.is_empty() {
            return Ok(need_irq);
        }

        for _ in 0..MAX_RX_FRAMES_PER_FLUSH {
            if self.rx_buffers.is_empty() {
                // Stop draining the backend once the guest has no posted buffers. This avoids
                // dropping frames that could be delivered later when more buffers arrive.
                break;
            }
            let Some(pkt) = self.backend.poll_receive() else {
                break;
            };
            // Contract v1: drop undersized/oversized Ethernet frames.
            if pkt.len() < 14 || pkt.len() > 1514 {
                continue;
            }

            // Find a posted RX buffer with enough writable capacity for header + payload.
            let needed = hdr_len + pkt.len();
            let Some(index) = self.rx_buffers.iter().position(|chain| {
                let descs = chain.descriptors();
                if descs.is_empty() || !descs.iter().all(|d| d.is_write_only()) {
                    return false;
                }
                if (descs[0].len as usize) < hdr_len {
                    return false;
                }
                let capacity = descs
                    .iter()
                    .map(|d| u64::from(d.len))
                    .fold(0u64, u64::saturating_add);
                capacity >= needed as u64
            }) else {
                // No buffer available: drop the frame.
                continue;
            };

            let Some(chain) = self.rx_buffers.remove(index) else {
                continue;
            };
            let descs = chain.descriptors();
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
                }
                if off < dst.len() && !remaining_pkt.is_empty() {
                    let take = remaining_pkt.len().min(dst.len() - off);
                    dst[off..off + take].copy_from_slice(&remaining_pkt[..take]);
                    remaining_pkt = &remaining_pkt[take..];
                }
                if header_off == hdr_len && remaining_pkt.is_empty() {
                    break;
                }
            }

            if header_off != hdr_len || !remaining_pkt.is_empty() {
                // Should not happen because we pre-flight capacity, but treat as a malformed
                // chain and complete it so the driver can recycle the buffer.
                need_irq |= queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                continue;
            }

            need_irq |= queue
                .add_used(mem, chain.head_index(), needed as u32)
                .map_err(|_| VirtioDeviceError::IoError)?;
        }

        Ok(need_irq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{read_u16_le, write_u16_le, write_u32_le, write_u64_le, GuestRam};
    use crate::queue::{
        PoppedDescriptorChain, VirtQueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
    };

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

        packet.extend(std::iter::repeat_n(0x42u8, payload_len));
        packet
    }

    #[test]
    fn device_features_match_win7_contract_v1() {
        let dev = VirtioNet::new(LoopbackNet::default(), [0; 6]);
        let features = <VirtioNet<LoopbackNet> as VirtioDevice>::device_features(&dev);
        assert_eq!(
            features,
            VIRTIO_F_VERSION_1
                | VIRTIO_F_RING_INDIRECT_DESC
                | VIRTIO_NET_F_MAC
                | VIRTIO_NET_F_STATUS
        );
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

        let packet = vec![0u8; 14];
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

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };
        let irq = dev.process_queue(1, chain, &mut queue, &mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.backend_mut().tx_packets, vec![packet]);

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

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };
        dev.process_queue(1, chain, &mut queue, &mut mem).unwrap();

        assert_eq!(dev.backend_mut().tx_packets.len(), 3);
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    }

    #[test]
    fn rx_poll_budget_prevents_infinite_backend_loop() {
        #[derive(Default)]
        struct InfiniteRxBackend {
            polls: usize,
        }

        impl NetBackend for InfiniteRxBackend {
            fn transmit(&mut self, _packet: Vec<u8>) {}

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.polls += 1;
                assert!(
                    self.polls <= MAX_RX_FRAMES_PER_FLUSH,
                    "flush_rx should not poll_receive() more than MAX_RX_FRAMES_PER_FLUSH times"
                );
                Some(vec![0u8; 14])
            }
        }

        let mut dev = VirtioNet::new(InfiniteRxBackend::default(), [0; 6]);
        dev.set_features(0);

        // Post one RX buffer that is valid (hdr fits) but too small for the payload. This keeps
        // `rx_buffers` non-empty so `flush_rx()` will attempt to poll the backend and will drop
        // packets due to no suitable buffer.
        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let buf_addr = 0x4000;
        let hdr_len = VirtioNetHdr::BASE_LEN as u32;
        write_desc(
            &mut mem,
            desc_table,
            0,
            buf_addr,
            hdr_len,
            VIRTQ_DESC_F_WRITE,
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

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };

        // Without a bound, this would loop forever because the backend never returns `None` and
        // the device has no suitable RX buffer to consume packets.
        dev.process_rx(chain, &mut queue, &mut mem).unwrap();
        assert_eq!(dev.backend_mut().polls, MAX_RX_FRAMES_PER_FLUSH);
    }

    #[test]
    fn rx_does_not_poll_backend_without_posted_buffers() {
        #[derive(Default)]
        struct CountingBackend {
            polls: usize,
        }

        impl NetBackend for CountingBackend {
            fn transmit(&mut self, _packet: Vec<u8>) {}

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.polls += 1;
                Some(vec![0u8; 14])
            }
        }

        let mut dev = VirtioNet::new(CountingBackend::default(), [0; 6]);
        let mut mem = GuestRam::new(0x1000);
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: 8,
                desc_addr: 0,
                avail_addr: 0,
                used_addr: 0,
            },
            false,
        )
        .unwrap();

        let irq = dev.flush_rx(&mut queue, &mut mem).unwrap();
        assert!(!irq);
        assert_eq!(dev.backend_mut().polls, 0);
    }

    #[test]
    fn rx_frames_are_delivered_after_buffers_are_posted() {
        #[derive(Default)]
        struct CountingBackend {
            polls: usize,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetBackend for CountingBackend {
            fn transmit(&mut self, _packet: Vec<u8>) {}

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.polls += 1;
                self.rx.pop_front()
            }
        }

        let mut backend = CountingBackend::default();
        let frame = vec![0x11u8; 14];
        backend.rx.push_back(frame.clone());

        let mut dev = VirtioNet::new(backend, [0; 6]);
        dev.set_features(0);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let buf_addr = 0x4000;

        write_desc(
            &mut mem,
            desc_table,
            0,
            buf_addr,
            64,
            VIRTQ_DESC_F_WRITE,
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

        // No RX buffers posted to the device yet, so the backend should not be polled.
        let irq = dev.flush_rx(&mut queue, &mut mem).unwrap();
        assert!(!irq);
        assert_eq!(dev.backend_mut().polls, 0);

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };

        // Posting a buffer should cause the pending backend frame to be delivered immediately.
        let irq = dev.process_rx(chain, &mut queue, &mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.backend_mut().polls, 1);

        let used_idx = read_u16_le(&mem, used + 2).unwrap();
        assert_eq!(used_idx, 1);
        let len_bytes = mem.get_slice(used + 8, 4).unwrap();
        let used_len = u32::from_le_bytes(len_bytes.try_into().unwrap());
        assert_eq!(
            used_len,
            (VirtioNetHdr::BASE_LEN + frame.len()) as u32
        );

        let hdr = mem.get_slice(buf_addr, VirtioNetHdr::BASE_LEN).unwrap();
        assert_eq!(hdr, vec![0u8; VirtioNetHdr::BASE_LEN]);
        let payload = mem
            .get_slice(buf_addr + VirtioNetHdr::BASE_LEN as u64, frame.len())
            .unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn rx_does_not_consume_extra_frames_when_buffers_run_out() {
        #[derive(Default)]
        struct CountingBackend {
            polls: usize,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetBackend for CountingBackend {
            fn transmit(&mut self, _packet: Vec<u8>) {}

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.polls += 1;
                self.rx.pop_front()
            }
        }

        let mut backend = CountingBackend::default();
        let frame1 = vec![0x11u8; 14];
        let frame2 = vec![0x22u8; 14];
        backend.rx.push_back(frame1.clone());
        backend.rx.push_back(frame2.clone());

        let mut dev = VirtioNet::new(backend, [0; 6]);
        dev.set_features(0);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let buf0 = 0x4000;
        let buf1 = 0x4100;

        write_desc(&mut mem, desc_table, 0, buf0, 64, VIRTQ_DESC_F_WRITE, 0);
        write_desc(&mut mem, desc_table, 1, buf1, 64, VIRTQ_DESC_F_WRITE, 0);

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

        let chain0 = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };
        dev.process_rx(chain0, &mut queue, &mut mem).unwrap();

        assert_eq!(dev.backend_mut().polls, 1);
        assert_eq!(dev.backend_mut().rx.len(), 1);
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        let payload = mem
            .get_slice(buf0 + VirtioNetHdr::BASE_LEN as u64, frame1.len())
            .unwrap();
        assert_eq!(payload, frame1);

        // Post a second buffer and ensure the second frame is still available to be delivered.
        write_u16_le(&mut mem, avail + 2, 2).unwrap();
        write_u16_le(&mut mem, avail + 4 + 2, 1).unwrap();
        let chain1 = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };
        dev.process_rx(chain1, &mut queue, &mut mem).unwrap();

        assert_eq!(dev.backend_mut().polls, 2);
        assert_eq!(dev.backend_mut().rx.len(), 0);
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 2);
        let payload = mem
            .get_slice(buf1 + VirtioNetHdr::BASE_LEN as u64, frame2.len())
            .unwrap();
        assert_eq!(payload, frame2);
    }

    #[test]
    fn rx_posted_buffer_queue_is_bounded() {
        // This simulates a malicious guest that bumps `avail.idx` far ahead so the transport keeps
        // popping descriptor chains for the same handful of ring entries. The device must not grow
        // `rx_buffers` without bound.
        let mut dev = VirtioNet::new(LoopbackNet::default(), [0; 6]);
        dev.set_features(0);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        // One write-only descriptor per head index, large enough for hdr+payload.
        for i in 0..qsize {
            let buf_addr = 0x4000 + u64::from(i) * 0x100;
            write_desc(&mut mem, desc_table, i, buf_addr, 64, VIRTQ_DESC_F_WRITE, 0);
        }

        // Malicious: claim there are 1000 available entries, but only provide `qsize` ring slots.
        let avail_idx = 1000u16;
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, avail_idx).unwrap();
        for i in 0..qsize {
            write_u16_le(&mut mem, avail + 4 + u64::from(i) * 2, i).unwrap();
        }
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        for _ in 0..avail_idx {
            let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
                PoppedDescriptorChain::Chain(chain) => chain,
                PoppedDescriptorChain::Invalid { error, .. } => {
                    panic!("unexpected descriptor chain parse error: {error:?}")
                }
            };
            dev.process_rx(chain, &mut queue, &mut mem).unwrap();
        }

        assert_eq!(dev.rx_buffers.len(), qsize as usize);
        assert_eq!(
            read_u16_le(&mem, used + 2).unwrap(),
            avail_idx - qsize,
            "extra RX buffers should be completed with used.len=0 once the internal queue is full"
        );
    }
}
