use crate::io::virtio::vio_core::{
    Descriptor, DescriptorChain, VirtQueue, VirtQueueError, VRING_DESC_F_WRITE,
};
use memory::GuestMemory;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioNetHeader {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
}

impl VirtioNetHeader {
    pub const SIZE: usize = 10;

    pub fn from_bytes(bytes: [u8; Self::SIZE]) -> Self {
        Self {
            flags: bytes[0],
            gso_type: bytes[1],
            hdr_len: u16::from_le_bytes([bytes[2], bytes[3]]),
            gso_size: u16::from_le_bytes([bytes[4], bytes[5]]),
            csum_start: u16::from_le_bytes([bytes[6], bytes[7]]),
            csum_offset: u16::from_le_bytes([bytes[8], bytes[9]]),
        }
    }

    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[0] = self.flags;
        out[1] = self.gso_type;
        out[2..4].copy_from_slice(&self.hdr_len.to_le_bytes());
        out[4..6].copy_from_slice(&self.gso_size.to_le_bytes());
        out[6..8].copy_from_slice(&self.csum_start.to_le_bytes());
        out[8..10].copy_from_slice(&self.csum_offset.to_le_bytes());
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetConfig {
    pub mac: [u8; 6],
    pub status: u16,
    pub max_queue_pairs: u16,
}

impl VirtioNetConfig {
    pub const SIZE: usize = 10;

    pub fn read(&self, offset: usize, data: &mut [u8]) {
        let mut config = [0u8; Self::SIZE];
        config[..6].copy_from_slice(&self.mac);
        config[6..8].copy_from_slice(&self.status.to_le_bytes());
        config[8..10].copy_from_slice(&self.max_queue_pairs.to_le_bytes());

        if offset >= config.len() {
            data.fill(0);
            return;
        }

        let end = usize::min(config.len(), offset + data.len());
        data[..end - offset].copy_from_slice(&config[offset..end]);
        if end - offset < data.len() {
            data[end - offset..].fill(0);
        }
    }
}

pub trait EthernetFrameSink {
    fn send(&mut self, frame: Vec<u8>);
}

#[derive(Debug)]
pub struct VirtioNetDevice {
    pub config: VirtioNetConfig,
    pub rx_vq: VirtQueue,
    pub tx_vq: VirtQueue,

    pending_rx: VecDeque<Vec<u8>>,
    isr_queue: bool,
}

impl VirtioNetDevice {
    pub fn new(config: VirtioNetConfig, rx_vq: VirtQueue, tx_vq: VirtQueue) -> Self {
        Self {
            config,
            rx_vq,
            tx_vq,
            pending_rx: VecDeque::new(),
            isr_queue: false,
        }
    }

    /// Equivalent to reading the virtio ISR status register: returns the current interrupt bits
    /// and clears them.
    pub fn take_isr(&mut self) -> u8 {
        let isr = if self.isr_queue { 0x1 } else { 0x0 };
        self.isr_queue = false;
        isr
    }

    /// Process transmit descriptors (guest → host).
    ///
    /// Returns `true` if the guest should be interrupted for queue updates.
    pub fn process_tx(
        &mut self,
        mem: &mut impl GuestMemory,
        sink: &mut impl EthernetFrameSink,
    ) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(chain) = self.tx_vq.pop_available(mem)? {
            if let Some(frame) = read_tx_frame(mem, &chain)? {
                sink.send(frame);
            }

            if self.tx_vq.push_used(mem, &chain, 0)? {
                should_interrupt = true;
            }
        }

        if should_interrupt {
            self.isr_queue = true;
        }

        Ok(should_interrupt)
    }

    /// Queue (and potentially immediately deliver) a received Ethernet frame (host → guest).
    ///
    /// Returns `true` if the guest should be interrupted for queue updates.
    pub fn inject_rx_frame(
        &mut self,
        mem: &mut impl GuestMemory,
        frame: &[u8],
    ) -> Result<bool, VirtQueueError> {
        self.pending_rx.push_back(frame.to_vec());
        self.process_pending_rx(mem)
    }

    /// Called when the guest notifies the receive queue.
    pub fn notify_rx(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        self.process_pending_rx(mem)
    }

    fn process_pending_rx(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(frame) = self.pending_rx.pop_front() {
            let chain = match self.rx_vq.pop_available(mem)? {
                Some(chain) => chain,
                None => {
                    self.pending_rx.push_front(frame);
                    break;
                }
            };

            let written = write_rx_frame(mem, &chain, &frame)?;

            if self.rx_vq.push_used(mem, &chain, written as u32)? {
                should_interrupt = true;
            }
        }

        if should_interrupt {
            self.isr_queue = true;
        }

        Ok(should_interrupt)
    }
}

fn read_tx_frame(
    mem: &impl GuestMemory,
    chain: &DescriptorChain,
) -> Result<Option<Vec<u8>>, VirtQueueError> {
    if chain
        .descriptors
        .iter()
        .any(|desc| desc.flags & VRING_DESC_F_WRITE != 0)
    {
        return Ok(None);
    }

    let total_len: usize = chain.descriptors.iter().map(|d| d.len as usize).sum();

    if total_len < VirtioNetHeader::SIZE {
        return Ok(None);
    }

    let mut hdr_bytes = [0u8; VirtioNetHeader::SIZE];
    if let Err(err) = read_chain_exact(mem, &chain.descriptors, 0, &mut hdr_bytes) {
        return match err {
            VirtQueueError::DescriptorChainTooShort { .. } => Ok(None),
            other => Err(other),
        };
    }
    let _hdr = VirtioNetHeader::from_bytes(hdr_bytes);

    let mut frame = vec![0u8; total_len - VirtioNetHeader::SIZE];
    if let Err(err) = read_chain_exact(mem, &chain.descriptors, VirtioNetHeader::SIZE, &mut frame) {
        return match err {
            VirtQueueError::DescriptorChainTooShort { .. } => Ok(None),
            other => Err(other),
        };
    }

    Ok(Some(frame))
}

fn write_rx_frame(
    mem: &mut impl GuestMemory,
    chain: &DescriptorChain,
    frame: &[u8],
) -> Result<usize, VirtQueueError> {
    let hdr = VirtioNetHeader::default().to_bytes();

    let header_written = write_chain(mem, &chain.descriptors, 0, &hdr)?;
    if header_written < hdr.len() {
        // The guest didn't provide enough space for the mandatory header.
        return Ok(header_written);
    }

    let packet_written = write_chain(mem, &chain.descriptors, hdr.len(), frame)?;
    Ok(hdr.len() + packet_written)
}

fn read_chain_exact(
    mem: &impl GuestMemory,
    descs: &[Descriptor],
    mut offset: usize,
    out: &mut [u8],
) -> Result<(), VirtQueueError> {
    let mut written = 0usize;

    for desc in descs {
        let desc_len = desc.len as usize;

        if offset >= desc_len {
            offset -= desc_len;
            continue;
        }

        let available = desc_len - offset;
        let to_read = usize::min(available, out.len() - written);
        mem.read_into(
            desc.addr + offset as u64,
            &mut out[written..written + to_read],
        )?;
        written += to_read;
        offset = 0;

        if written == out.len() {
            return Ok(());
        }
    }

    Err(VirtQueueError::DescriptorChainTooShort {
        requested: out.len(),
    })
}

fn write_chain(
    mem: &mut impl GuestMemory,
    descs: &[Descriptor],
    mut offset: usize,
    data: &[u8],
) -> Result<usize, VirtQueueError> {
    let mut remaining = data;
    let mut written = 0usize;

    for desc in descs {
        if desc.flags & VRING_DESC_F_WRITE == 0 {
            if offset == 0 {
                break;
            }
        }

        let desc_len = desc.len as usize;

        if offset >= desc_len {
            offset -= desc_len;
            continue;
        }

        if desc.flags & VRING_DESC_F_WRITE == 0 {
            break;
        }

        let available = desc_len - offset;
        let to_write = usize::min(available, remaining.len());
        mem.write_from(desc.addr + offset as u64, &remaining[..to_write])?;
        written += to_write;
        remaining = &remaining[to_write..];
        offset = 0;

        if remaining.is_empty() {
            break;
        }
    }

    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::virtio::vio_core::{VRING_AVAIL_F_NO_INTERRUPT, VRING_DESC_F_NEXT};
    use memory::DenseMemory;

    struct CaptureSink {
        frames: Vec<Vec<u8>>,
    }

    impl CaptureSink {
        fn new() -> Self {
            Self { frames: Vec::new() }
        }
    }

    impl EthernetFrameSink for CaptureSink {
        fn send(&mut self, frame: Vec<u8>) {
            self.frames.push(frame);
        }
    }

    fn write_desc(mem: &mut DenseMemory, base: u64, index: u16, desc: Descriptor) {
        let off = base + (index as u64) * 16;
        mem.write_u64_le(off, desc.addr).unwrap();
        mem.write_u32_le(off + 8, desc.len).unwrap();
        mem.write_u16_le(off + 12, desc.flags).unwrap();
        mem.write_u16_le(off + 14, desc.next).unwrap();
    }

    fn init_avail(mem: &mut DenseMemory, avail: u64, flags: u16, head: u16) {
        mem.write_u16_le(avail, flags).unwrap();
        mem.write_u16_le(avail + 2, 1).unwrap();
        mem.write_u16_le(avail + 4, head).unwrap();
    }

    #[test]
    fn tx_chain_yields_outgoing_bytes() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x110;
        let payload_addr = 0x200;

        let header = VirtioNetHeader::default().to_bytes();
        mem.write_from(header_addr, &header).unwrap();

        let payload = b"\xaa\xbb\xcc\xdd\xee";
        mem.write_from(payload_addr, payload).unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header_addr,
                len: header.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );

        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: payload_addr,
                len: payload.len() as u32,
                flags: 0,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 0);
        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();

        let rx_vq = VirtQueue::new(8, 0, 0, 0);
        let tx_vq = VirtQueue::new(8, desc_table, avail, used);

        let config = VirtioNetConfig {
            mac: [0; 6],
            status: 1,
            max_queue_pairs: 1,
        };
        let mut dev = VirtioNetDevice::new(config, rx_vq, tx_vq);

        let mut sink = CaptureSink::new();
        let irq = dev.process_tx(&mut mem, &mut sink).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(sink.frames, vec![payload.to_vec()]);

        let used_idx = mem.read_u16_le(used + 2).unwrap();
        assert_eq!(used_idx, 1);

        let used_id = mem.read_u32_le(used + 4).unwrap();
        let used_len = mem.read_u32_le(used + 8).unwrap();
        assert_eq!(used_id, 0);
        assert_eq!(used_len, 0);
    }

    #[test]
    fn rx_injection_fills_buffers_and_reports_used_length() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x400;
        let payload_addr = 0x500;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 1,
            },
        );

        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: payload_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 0);
        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();

        let rx_vq = VirtQueue::new(8, desc_table, avail, used);
        let tx_vq = VirtQueue::new(8, 0, 0, 0);

        let config = VirtioNetConfig {
            mac: [0; 6],
            status: 1,
            max_queue_pairs: 1,
        };

        let mut dev = VirtioNetDevice::new(config, rx_vq, tx_vq);

        let frame = b"\x01\x02\x03\x04";
        let irq = dev.inject_rx_frame(&mut mem, frame).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut hdr_bytes = [0u8; VirtioNetHeader::SIZE];
        mem.read_into(header_addr, &mut hdr_bytes).unwrap();
        assert_eq!(hdr_bytes, VirtioNetHeader::default().to_bytes());

        let mut payload = [0u8; 4];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(&payload, frame);

        let used_idx = mem.read_u16_le(used + 2).unwrap();
        assert_eq!(used_idx, 1);

        let used_id = mem.read_u32_le(used + 4).unwrap();
        let used_len = mem.read_u32_le(used + 8).unwrap();
        assert_eq!(used_id, 0);
        assert_eq!(used_len, (VirtioNetHeader::SIZE + frame.len()) as u32);
    }

    #[test]
    fn tx_respects_no_interrupt_flag() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x110;
        let payload_addr = 0x200;

        mem.write_from(header_addr, &VirtioNetHeader::default().to_bytes())
            .unwrap();
        mem.write_from(payload_addr, b"\x99").unwrap();

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: payload_addr,
                len: 1,
                flags: 0,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, VRING_AVAIL_F_NO_INTERRUPT, 0);
        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();

        let rx_vq = VirtQueue::new(8, 0, 0, 0);
        let tx_vq = VirtQueue::new(8, desc_table, avail, used);
        let config = VirtioNetConfig {
            mac: [0; 6],
            status: 1,
            max_queue_pairs: 1,
        };
        let mut dev = VirtioNetDevice::new(config, rx_vq, tx_vq);

        let mut sink = CaptureSink::new();
        let irq = dev.process_tx(&mut mem, &mut sink).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);
    }

    #[test]
    fn rx_queues_frames_until_buffers_arrive() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        mem.write_u16_le(avail, 0).unwrap();
        mem.write_u16_le(avail + 2, 0).unwrap();

        let rx_vq = VirtQueue::new(8, desc_table, avail, used);
        let tx_vq = VirtQueue::new(8, 0, 0, 0);
        let config = VirtioNetConfig {
            mac: [0; 6],
            status: 1,
            max_queue_pairs: 1,
        };
        let mut dev = VirtioNetDevice::new(config, rx_vq, tx_vq);

        mem.write_u16_le(used, 0).unwrap();
        mem.write_u16_le(used + 2, 0).unwrap();

        let frame = b"\xaa\xbb\xcc";
        let irq = dev.inject_rx_frame(&mut mem, frame).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);

        let header_addr = 0x400;
        let payload_addr = 0x500;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            Descriptor {
                addr: payload_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, avail, 0, 0);

        let irq = dev.notify_rx(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut payload = [0u8; 3];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(&payload, frame);
    }
}
