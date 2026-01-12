use crate::io::net::NetworkBackend;
use crate::io::virtio::vio_core::{
    Descriptor, DescriptorChain, VirtQueue, VirtQueueError, VRING_DESC_F_WRITE,
};
use memory::{GuestMemory, GuestMemoryError};
use std::collections::VecDeque;

const MIN_FRAME_LEN: usize = 14;
const MAX_FRAME_LEN: usize = 1522;
const MAX_TX_TOTAL_LEN: usize = VirtioNetHeader::SIZE + MAX_FRAME_LEN;
/// Upper bound on queued host→guest frames, to avoid unbounded memory growth if the guest never
/// posts RX buffers.
const MAX_PENDING_RX_FRAMES: usize = 256;

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
        backend: &mut impl NetworkBackend,
    ) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(chain) = self.tx_vq.pop_available(mem)? {
            if let Some(frame) = read_tx_frame(mem, &chain)? {
                backend.transmit(frame);
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
        if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&frame.len()) {
            return Ok(false);
        }

        let mut should_interrupt = false;

        // Flush any pending frames first so we don't drop older packets unnecessarily when the
        // queue is at capacity but RX buffers are now available.
        if self.process_pending_rx(mem)? {
            should_interrupt = true;
        }

        self.enqueue_rx_frame(frame.to_vec());

        if self.process_pending_rx(mem)? {
            should_interrupt = true;
        }

        Ok(should_interrupt)
    }

    pub fn enqueue_rx_frame(&mut self, frame: Vec<u8>) {
        if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&frame.len()) {
            return;
        }

        if self.pending_rx.len() >= MAX_PENDING_RX_FRAMES {
            self.pending_rx.pop_front();
        }
        self.pending_rx.push_back(frame);
    }

    pub fn poll(
        &mut self,
        mem: &mut impl GuestMemory,
        backend: &mut impl NetworkBackend,
    ) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        if self.process_tx(mem, backend)? {
            should_interrupt = true;
        }

        // Flush any already-queued frames before draining the backend so we don't
        // drop older packets unnecessarily when `pending_rx` hits its limit.
        if self.process_pending_rx(mem)? {
            should_interrupt = true;
        }

        while let Some(frame) = backend.poll_receive() {
            self.enqueue_rx_frame(frame);
            if self.process_pending_rx(mem)? {
                should_interrupt = true;
            }
        }

        if self.process_pending_rx(mem)? {
            should_interrupt = true;
        }

        Ok(should_interrupt)
    }

    /// Called when the guest notifies the receive queue.
    pub fn notify_rx(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        self.process_pending_rx(mem)
    }

    fn process_pending_rx(&mut self, mem: &mut impl GuestMemory) -> Result<bool, VirtQueueError> {
        let mut should_interrupt = false;

        while let Some(frame) = self.pending_rx.front() {
            if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&frame.len()) {
                self.pending_rx.pop_front();
                continue;
            }

            let chain = match self.rx_vq.peek_available(mem)? {
                Some(chain) => chain,
                None => break,
            };

            if !rx_chain_can_fit_frame(mem, &chain, frame.len()) {
                // Buffers are insufficient; drop without consuming the RX chain.
                self.pending_rx.pop_front();
                continue;
            }

            if !self.rx_vq.consume_available(mem)? {
                break;
            }

            // Safe unwrap: `frame` came from `front()` above and hasn't been popped yet.
            let frame = self.pending_rx.pop_front().unwrap();
            write_rx_frame(mem, &chain, &frame)?;

            if self
                .rx_vq
                .push_used(mem, &chain, (VirtioNetHeader::SIZE + frame.len()) as u32)?
            {
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

    let total_len = match tx_total_len(&chain.descriptors) {
        Some(total_len) => total_len,
        None => return Ok(None),
    };

    if total_len < VirtioNetHeader::SIZE + MIN_FRAME_LEN {
        return Ok(None);
    }

    if total_len > MAX_TX_TOTAL_LEN {
        return Ok(None);
    }

    let frame_len = total_len - VirtioNetHeader::SIZE;
    let mut frame = vec![0u8; frame_len];
    if let Err(err) = read_chain_exact(mem, &chain.descriptors, VirtioNetHeader::SIZE, &mut frame) {
        return match err {
            VirtQueueError::DescriptorChainTooShort { .. } => Ok(None),
            VirtQueueError::GuestMemory(_) => Ok(None),
            other => Err(other),
        };
    }

    Ok(Some(frame))
}

fn write_rx_frame(
    mem: &mut impl GuestMemory,
    chain: &DescriptorChain,
    frame: &[u8],
) -> Result<(), VirtQueueError> {
    let hdr = VirtioNetHeader::default().to_bytes();

    let header_written = write_chain(mem, &chain.descriptors, 0, &hdr)?;
    if header_written < hdr.len() {
        return Ok(());
    }

    write_chain(mem, &chain.descriptors, hdr.len(), frame)?;
    Ok(())
}

fn tx_total_len(descs: &[Descriptor]) -> Option<usize> {
    let mut total = 0usize;
    for desc in descs {
        if total > MAX_TX_TOTAL_LEN {
            return Some(total);
        }
        total = total.checked_add(desc.len as usize)?;
    }
    Some(total)
}

fn rx_chain_can_fit_frame(
    mem: &impl GuestMemory,
    chain: &DescriptorChain,
    frame_len: usize,
) -> bool {
    let needed = VirtioNetHeader::SIZE + frame_len;
    if needed > MAX_TX_TOTAL_LEN {
        return false;
    }

    let mem_size = mem.size();

    let Some(first) = chain.descriptors.first() else {
        return false;
    };

    if first.flags & VRING_DESC_F_WRITE == 0 || (first.len as usize) < VirtioNetHeader::SIZE {
        return false;
    }

    let header_end = match first.addr.checked_add(VirtioNetHeader::SIZE as u64) {
        Some(end) => end,
        None => return false,
    };
    if header_end > mem_size {
        return false;
    }

    let mut remaining_payload = frame_len;
    let mut offset = VirtioNetHeader::SIZE;
    for desc in &chain.descriptors {
        if desc.flags & VRING_DESC_F_WRITE == 0 {
            break;
        }

        let desc_len = desc.len as usize;
        if offset >= desc_len {
            offset -= desc_len;
            continue;
        }

        let available = desc_len - offset;
        let to_write = usize::min(available, remaining_payload);
        if to_write > 0 {
            let addr = match desc.addr.checked_add(offset as u64) {
                Some(addr) => addr,
                None => return false,
            };
            let end = match addr.checked_add(to_write as u64) {
                Some(end) => end,
                None => return false,
            };
            if end > mem_size {
                return false;
            }
            remaining_payload -= to_write;
        }

        offset = 0;
        if remaining_payload == 0 {
            return true;
        }
    }

    false
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
        let addr = desc.addr.checked_add(offset as u64).ok_or_else(|| {
            VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange {
                paddr: desc.addr,
                len: to_read,
                size: mem.size(),
            })
        })?;
        mem.read_into(addr, &mut out[written..written + to_read])?;
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
        if desc.flags & VRING_DESC_F_WRITE == 0 && offset == 0 {
            break;
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
        let addr = desc.addr.checked_add(offset as u64).ok_or_else(|| {
            VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange {
                paddr: desc.addr,
                len: to_write,
                size: mem.size(),
            })
        })?;
        mem.write_from(addr, &remaining[..to_write])?;
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
    use crate::io::net::L2TunnelBackend;
    use crate::io::net::L2TunnelRingBackend;
    use crate::io::virtio::vio_core::{VRING_AVAIL_F_NO_INTERRUPT, VRING_DESC_F_NEXT};
    use aero_ipc::ring::RingBuffer;
    use memory::DenseMemory;
    use std::sync::Arc;

    #[derive(Default)]
    struct TestBackend {
        frames: Vec<Vec<u8>>,
        rx_frames: std::collections::VecDeque<Vec<u8>>,
    }

    impl NetworkBackend for TestBackend {
        fn transmit(&mut self, frame: Vec<u8>) {
            self.frames.push(frame);
        }

        fn poll_receive(&mut self) -> Option<Vec<u8>> {
            self.rx_frames.pop_front()
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

    fn init_avail_multi(mem: &mut DenseMemory, avail: u64, flags: u16, heads: &[u16]) {
        mem.write_u16_le(avail, flags).unwrap();
        mem.write_u16_le(avail + 2, heads.len() as u16).unwrap();
        for (idx, head) in heads.iter().enumerate() {
            mem.write_u16_le(avail + 4 + (idx as u64) * 2, *head)
                .unwrap();
        }
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

        let payload = [0xaau8; MIN_FRAME_LEN];
        mem.write_from(payload_addr, &payload).unwrap();

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

        let mut backend = TestBackend::default();
        let irq = dev.process_tx(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(backend.frames, vec![payload.to_vec()]);

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

        let frame = [0x11u8; MIN_FRAME_LEN];
        let irq = dev.inject_rx_frame(&mut mem, &frame).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut hdr_bytes = [0u8; VirtioNetHeader::SIZE];
        mem.read_into(header_addr, &mut hdr_bytes).unwrap();
        assert_eq!(hdr_bytes, VirtioNetHeader::default().to_bytes());

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);

        let used_idx = mem.read_u16_le(used + 2).unwrap();
        assert_eq!(used_idx, 1);

        let used_id = mem.read_u32_le(used + 4).unwrap();
        let used_len = mem.read_u32_le(used + 8).unwrap();
        assert_eq!(used_id, 0);
        assert_eq!(used_len, (VirtioNetHeader::SIZE + frame.len()) as u32);
    }

    #[test]
    fn inject_rx_frame_flushes_pending_before_enqueuing_to_avoid_drop_at_capacity() {
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

        for idx in 0..MAX_PENDING_RX_FRAMES {
            let mut frame = vec![0u8; MIN_FRAME_LEN];
            frame[0] = (idx & 0xff) as u8;
            frame[1] = ((idx >> 8) & 0xff) as u8;
            dev.enqueue_rx_frame(frame);
        }

        let injected = vec![0xffu8; MIN_FRAME_LEN];
        let irq = dev.inject_rx_frame(&mut mem, &injected).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        // The oldest queued frame should be delivered, not dropped to make space for `injected`.
        let mut expected = vec![0u8; MIN_FRAME_LEN];
        expected[0] = 0;
        expected[1] = 0;

        let mut payload = vec![0u8; expected.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, expected);
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
        mem.write_from(payload_addr, &[0x99u8; MIN_FRAME_LEN])
            .unwrap();

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
                len: MIN_FRAME_LEN as u32,
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

        let mut backend = TestBackend::default();
        let irq = dev.process_tx(&mut mem, &mut backend).unwrap();
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

        let frame = [0xabu8; MIN_FRAME_LEN];
        let irq = dev.inject_rx_frame(&mut mem, &frame).unwrap();
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

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn poll_queues_backend_frames_until_buffers_arrive() {
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

        let frame = vec![0xabu8; MIN_FRAME_LEN];
        let mut backend = TestBackend::default();
        backend.rx_frames.push_back(frame.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);
        assert!(backend.rx_frames.is_empty());
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 0);

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

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn poll_drains_backend_receive_and_delivers_into_rx_buffers() {
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

        let frame = vec![0x11u8; MIN_FRAME_LEN];
        let mut backend = TestBackend::default();
        backend.rx_frames.push_back(frame.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut hdr_bytes = [0u8; VirtioNetHeader::SIZE];
        mem.read_into(header_addr, &mut hdr_bytes).unwrap();
        assert_eq!(hdr_bytes, VirtioNetHeader::default().to_bytes());

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn poll_drains_multiple_backend_frames_in_fifo_order() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header0_addr = 0x400;
        let payload0_addr = 0x500;
        let header2_addr = 0x600;
        let payload2_addr = 0x700;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header0_addr,
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
                addr: payload0_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            Descriptor {
                addr: header2_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            Descriptor {
                addr: payload2_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail_multi(&mut mem, avail, 0, &[0, 2]);
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

        let frame0 = vec![0x11u8; MIN_FRAME_LEN];
        let frame1 = vec![0x22u8; MIN_FRAME_LEN];

        let mut backend = TestBackend::default();
        backend.rx_frames.push_back(frame0.clone());
        backend.rx_frames.push_back(frame1.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 2);

        let used0_id = mem.read_u32_le(used + 4).unwrap();
        let used0_len = mem.read_u32_le(used + 8).unwrap();
        assert_eq!(used0_id, 0);
        assert_eq!(used0_len, (VirtioNetHeader::SIZE + frame0.len()) as u32);

        let used1_id = mem.read_u32_le(used + 12).unwrap();
        let used1_len = mem.read_u32_le(used + 16).unwrap();
        assert_eq!(used1_id, 2);
        assert_eq!(used1_len, (VirtioNetHeader::SIZE + frame1.len()) as u32);

        let mut payload0 = vec![0u8; frame0.len()];
        mem.read_into(payload0_addr, &mut payload0).unwrap();
        assert_eq!(payload0, frame0);

        let mut payload1 = vec![0u8; frame1.len()];
        mem.read_into(payload2_addr, &mut payload1).unwrap();
        assert_eq!(payload1, frame1);
    }

    #[test]
    fn poll_drains_l2_tunnel_backend_receive_and_delivers_into_rx_buffers() {
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

        let frame = vec![0x11u8; MIN_FRAME_LEN];
        let mut backend = L2TunnelBackend::new();
        backend.push_rx_frame(frame.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn poll_drains_l2_tunnel_backend_frames_in_fifo_order() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header0_addr = 0x400;
        let payload0_addr = 0x500;
        let header2_addr = 0x600;
        let payload2_addr = 0x700;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header0_addr,
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
                addr: payload0_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            Descriptor {
                addr: header2_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            Descriptor {
                addr: payload2_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail_multi(&mut mem, avail, 0, &[0, 2]);
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

        let frame0 = vec![0x11u8; MIN_FRAME_LEN];
        let frame1 = vec![0x22u8; MIN_FRAME_LEN];

        let mut backend = L2TunnelBackend::new();
        backend.push_rx_frame(frame0.clone());
        backend.push_rx_frame(frame1.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 2);

        let used0_id = mem.read_u32_le(used + 4).unwrap();
        let used1_id = mem.read_u32_le(used + 12).unwrap();
        assert_eq!(used0_id, 0);
        assert_eq!(used1_id, 2);

        let mut payload0 = vec![0u8; frame0.len()];
        mem.read_into(payload0_addr, &mut payload0).unwrap();
        assert_eq!(payload0, frame0);

        let mut payload1 = vec![0u8; frame1.len()];
        mem.read_into(payload2_addr, &mut payload1).unwrap();
        assert_eq!(payload1, frame1);
    }

    #[test]
    fn poll_drains_l2_tunnel_ring_backend_receive_and_delivers_into_rx_buffers() {
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

        let tx_ring = Arc::new(RingBuffer::new(64));
        let rx_ring = Arc::new(RingBuffer::new(64));
        let mut backend = L2TunnelRingBackend::new(tx_ring, rx_ring.clone());

        let frame = vec![0x11u8; MIN_FRAME_LEN];
        rx_ring.try_push(&frame).unwrap();

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn poll_drains_l2_tunnel_ring_backend_frames_in_fifo_order() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header0_addr = 0x400;
        let payload0_addr = 0x500;
        let header2_addr = 0x600;
        let payload2_addr = 0x700;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header0_addr,
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
                addr: payload0_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            Descriptor {
                addr: header2_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            Descriptor {
                addr: payload2_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail_multi(&mut mem, avail, 0, &[0, 2]);
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

        let tx_ring = Arc::new(RingBuffer::new(128));
        let rx_ring = Arc::new(RingBuffer::new(128));
        let mut backend = L2TunnelRingBackend::new(tx_ring, rx_ring.clone());

        let frame0 = vec![0x11u8; MIN_FRAME_LEN];
        let frame1 = vec![0x22u8; MIN_FRAME_LEN];
        rx_ring.try_push(&frame0).unwrap();
        rx_ring.try_push(&frame1).unwrap();

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 2);

        let used0_id = mem.read_u32_le(used + 4).unwrap();
        let used1_id = mem.read_u32_le(used + 12).unwrap();
        assert_eq!(used0_id, 0);
        assert_eq!(used1_id, 2);

        let mut payload0 = vec![0u8; frame0.len()];
        mem.read_into(payload0_addr, &mut payload0).unwrap();
        assert_eq!(payload0, frame0);

        let mut payload1 = vec![0u8; frame1.len()];
        mem.read_into(payload2_addr, &mut payload1).unwrap();
        assert_eq!(payload1, frame1);
    }

    #[test]
    fn poll_queues_excess_backend_frames_and_delivers_when_buffers_arrive_fifo() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header0_addr = 0x400;
        let payload0_addr = 0x500;
        let header2_addr = 0x600;
        let payload2_addr = 0x700;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header0_addr,
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
                addr: payload0_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            Descriptor {
                addr: header2_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            Descriptor {
                addr: payload2_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail_multi(&mut mem, avail, 0, &[0]);
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

        let frame0 = vec![0x11u8; MIN_FRAME_LEN];
        let frame1 = vec![0x22u8; MIN_FRAME_LEN];

        let mut backend = TestBackend::default();
        backend.rx_frames.push_back(frame0.clone());
        backend.rx_frames.push_back(frame1.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert!(backend.rx_frames.is_empty());
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 1);

        let mut payload0 = vec![0u8; frame0.len()];
        mem.read_into(payload0_addr, &mut payload0).unwrap();
        assert_eq!(payload0, frame0);

        init_avail_multi(&mut mem, avail, 0, &[0, 2]);
        let irq = dev.notify_rx(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 2);

        let mut payload1 = vec![0u8; frame1.len()];
        mem.read_into(payload2_addr, &mut payload1).unwrap();
        assert_eq!(payload1, frame1);
    }

    #[derive(Debug)]
    struct TxResponseBackend {
        tx_frames: Vec<Vec<u8>>,
        rx_frames: std::collections::VecDeque<Vec<u8>>,
        response: Vec<u8>,
    }

    impl TxResponseBackend {
        fn new(response: Vec<u8>) -> Self {
            Self {
                tx_frames: Vec::new(),
                rx_frames: std::collections::VecDeque::new(),
                response,
            }
        }
    }

    impl NetworkBackend for TxResponseBackend {
        fn transmit(&mut self, frame: Vec<u8>) {
            self.tx_frames.push(frame);
            self.rx_frames.push_back(self.response.clone());
        }

        fn poll_receive(&mut self) -> Option<Vec<u8>> {
            self.rx_frames.pop_front()
        }
    }

    #[test]
    fn poll_processes_tx_then_delivers_backend_response_in_same_tick() {
        let mut mem = DenseMemory::new(0x10_000).unwrap();

        let tx_desc_table = 0x1000;
        let tx_avail = 0x2000;
        let tx_used = 0x3000;

        let rx_desc_table = 0x4000;
        let rx_avail = 0x5000;
        let rx_used = 0x6000;

        let tx_header_addr = 0x110;
        let tx_payload_addr = 0x200;

        let tx_header = VirtioNetHeader::default().to_bytes();
        mem.write_from(tx_header_addr, &tx_header).unwrap();
        let tx_payload = [0x99u8; MIN_FRAME_LEN];
        mem.write_from(tx_payload_addr, &tx_payload).unwrap();

        write_desc(
            &mut mem,
            tx_desc_table,
            0,
            Descriptor {
                addr: tx_header_addr,
                len: tx_header.len() as u32,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            tx_desc_table,
            1,
            Descriptor {
                addr: tx_payload_addr,
                len: tx_payload.len() as u32,
                flags: 0,
                next: 0,
            },
        );

        init_avail(&mut mem, tx_avail, 0, 0);
        mem.write_u16_le(tx_used, 0).unwrap();
        mem.write_u16_le(tx_used + 2, 0).unwrap();

        let rx_header_addr = 0x700;
        let rx_payload_addr = 0x800;

        write_desc(
            &mut mem,
            rx_desc_table,
            0,
            Descriptor {
                addr: rx_header_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 1,
            },
        );
        write_desc(
            &mut mem,
            rx_desc_table,
            1,
            Descriptor {
                addr: rx_payload_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        init_avail(&mut mem, rx_avail, 0, 0);
        mem.write_u16_le(rx_used, 0).unwrap();
        mem.write_u16_le(rx_used + 2, 0).unwrap();

        let rx_vq = VirtQueue::new(8, rx_desc_table, rx_avail, rx_used);
        let tx_vq = VirtQueue::new(8, tx_desc_table, tx_avail, tx_used);
        let config = VirtioNetConfig {
            mac: [0; 6],
            status: 1,
            max_queue_pairs: 1,
        };
        let mut dev = VirtioNetDevice::new(config, rx_vq, tx_vq);

        let response = vec![0xabu8; MIN_FRAME_LEN];
        let mut backend = TxResponseBackend::new(response.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        assert_eq!(backend.tx_frames, vec![tx_payload.to_vec()]);
        assert_eq!(mem.read_u16_le(tx_used + 2).unwrap(), 1);
        assert_eq!(mem.read_u16_le(rx_used + 2).unwrap(), 1);

        let mut rx_hdr_bytes = [0u8; VirtioNetHeader::SIZE];
        mem.read_into(rx_header_addr, &mut rx_hdr_bytes).unwrap();
        assert_eq!(rx_hdr_bytes, VirtioNetHeader::default().to_bytes());

        let mut rx_payload = vec![0u8; response.len()];
        mem.read_into(rx_payload_addr, &mut rx_payload).unwrap();
        assert_eq!(rx_payload, response);
    }

    #[test]
    fn poll_receive_rx_respects_no_interrupt_flag() {
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

        init_avail(&mut mem, avail, VRING_AVAIL_F_NO_INTERRUPT, 0);
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

        let frame = vec![0x11u8; MIN_FRAME_LEN];
        let mut backend = TestBackend::default();
        backend.rx_frames.push_back(frame.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);

        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 1);

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn poll_receive_drops_invalid_frames_and_still_delivers_valid_ones() {
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

        let mut backend = TestBackend::default();
        backend.rx_frames.push_back(vec![0x55u8; MAX_FRAME_LEN + 1]); // oversized
        backend.rx_frames.push_back(vec![0x66u8; MIN_FRAME_LEN - 1]); // undersized

        let frame = vec![0x77u8; MIN_FRAME_LEN];
        backend.rx_frames.push_back(frame.clone());

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);
        assert!(backend.rx_frames.is_empty());

        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 1);
        assert_eq!(
            mem.read_u32_le(used + 8).unwrap(),
            (VirtioNetHeader::SIZE + frame.len()) as u32
        );

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn pending_rx_is_bounded_when_polling_backend() {
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

        let mut backend = TestBackend::default();
        for idx in 0..=MAX_PENDING_RX_FRAMES {
            let mut frame = vec![0u8; MIN_FRAME_LEN];
            frame[0] = (idx & 0xff) as u8;
            frame[1] = ((idx >> 8) & 0xff) as u8;
            backend.rx_frames.push_back(frame);
        }

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);
        assert!(backend.rx_frames.is_empty());

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

        let expected_idx = 1usize; // first frame should be dropped once the queue is full
        let mut expected = vec![0u8; MIN_FRAME_LEN];
        expected[0] = (expected_idx & 0xff) as u8;
        expected[1] = ((expected_idx >> 8) & 0xff) as u8;

        let mut payload = vec![0u8; expected.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, expected);
    }

    #[test]
    fn poll_flushes_pending_before_draining_backend_to_avoid_dropping_old_frames() {
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

        for idx in 0..MAX_PENDING_RX_FRAMES {
            let mut frame = vec![0u8; MIN_FRAME_LEN];
            frame[0] = (idx & 0xff) as u8;
            frame[1] = ((idx >> 8) & 0xff) as u8;
            dev.enqueue_rx_frame(frame);
        }

        let mut backend_frame = vec![0u8; MIN_FRAME_LEN];
        backend_frame[0] = (MAX_PENDING_RX_FRAMES & 0xff) as u8;
        backend_frame[1] = ((MAX_PENDING_RX_FRAMES >> 8) & 0xff) as u8;

        let mut backend = TestBackend::default();
        backend.rx_frames.push_back(backend_frame);

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);
        assert!(backend.rx_frames.is_empty());

        let mut payload = vec![0u8; MIN_FRAME_LEN];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload[0], 0);
        assert_eq!(payload[1], 0);
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 1);
    }

    #[test]
    fn poll_delivers_available_buffers_before_pending_queue_overflows() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header0_addr = 0x400;
        let payload0_addr = 0x500;
        let header2_addr = 0x600;
        let payload2_addr = 0x700;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: header0_addr,
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
                addr: payload0_addr,
                len: 64,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            Descriptor {
                addr: header2_addr,
                len: VirtioNetHeader::SIZE as u32,
                flags: VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
                next: 3,
            },
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            Descriptor {
                addr: payload2_addr,
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

        let mut backend = TestBackend::default();
        for idx in 0..=MAX_PENDING_RX_FRAMES {
            let mut frame = vec![0u8; MIN_FRAME_LEN];
            frame[0] = (idx & 0xff) as u8;
            frame[1] = ((idx >> 8) & 0xff) as u8;
            backend.rx_frames.push_back(frame);
        }

        let irq = dev.poll(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);
        assert!(backend.rx_frames.is_empty());

        let mut payload0 = vec![0u8; MIN_FRAME_LEN];
        mem.read_into(payload0_addr, &mut payload0).unwrap();
        assert_eq!(payload0[0], 0);
        assert_eq!(payload0[1], 0);

        init_avail_multi(&mut mem, avail, 0, &[0, 2]);
        let irq = dev.notify_rx(&mut mem).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut payload1 = vec![0u8; MIN_FRAME_LEN];
        mem.read_into(payload2_addr, &mut payload1).unwrap();
        assert_eq!(payload1[0], 1);
        assert_eq!(payload1[1], 0);
    }

    #[test]
    fn tx_drops_undersized_frames_but_completes_used_ring() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x110;
        let payload_addr = 0x200;

        mem.write_from(header_addr, &VirtioNetHeader::default().to_bytes())
            .unwrap();
        mem.write_from(payload_addr, &[0x22u8; MIN_FRAME_LEN - 1])
            .unwrap();

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
                len: (MIN_FRAME_LEN - 1) as u32,
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

        let mut backend = TestBackend::default();
        let irq = dev.process_tx(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);
        assert!(backend.frames.is_empty());

        let used_idx = mem.read_u16_le(used + 2).unwrap();
        assert_eq!(used_idx, 1);
    }

    #[test]
    fn tx_drops_oversized_frames_without_reading_payload() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x110;
        let oob_payload_addr = 0x5000;

        mem.write_from(header_addr, &VirtioNetHeader::default().to_bytes())
            .unwrap();

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
                addr: oob_payload_addr,
                len: (MAX_FRAME_LEN as u32) + 1,
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

        let mut backend = TestBackend::default();
        let irq = dev.process_tx(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);
        assert!(backend.frames.is_empty());

        let used_idx = mem.read_u16_le(used + 2).unwrap();
        assert_eq!(used_idx, 1);
    }

    #[test]
    fn tx_drops_on_length_sum_overflow() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x110;
        let oob_payload_addr = 0x5000;

        mem.write_from(header_addr, &VirtioNetHeader::default().to_bytes())
            .unwrap();

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
                addr: oob_payload_addr,
                len: u32::MAX,
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

        let mut backend = TestBackend::default();
        let irq = dev.process_tx(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert!(backend.frames.is_empty());

        let used_idx = mem.read_u16_le(used + 2).unwrap();
        assert_eq!(used_idx, 1);
    }

    #[test]
    fn rx_drops_oversized_injected_frames_without_consuming_buffers() {
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
                len: 2048,
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

        let oversized_frame = vec![0x55u8; MAX_FRAME_LEN + 1];
        let irq = dev.inject_rx_frame(&mut mem, &oversized_frame).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 0);

        let frame = [0x66u8; MIN_FRAME_LEN];
        let irq = dev.inject_rx_frame(&mut mem, &frame).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 1);

        let mut payload = vec![0u8; frame.len()];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(payload, frame);
    }

    #[test]
    fn rx_does_not_deliver_truncated_frames_when_buffers_too_small() {
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
                len: 20,
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

        let too_large = vec![0xaau8; 25];
        let irq = dev.inject_rx_frame(&mut mem, &too_large).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 0);

        let frame = [0xbbu8; MIN_FRAME_LEN];
        let irq = dev.inject_rx_frame(&mut mem, &frame).unwrap();
        assert!(irq);
        assert_eq!(dev.take_isr(), 0x1);

        let mut payload = vec![0u8; 20];
        mem.read_into(payload_addr, &mut payload).unwrap();
        assert_eq!(&payload[..frame.len()], frame);
        assert_eq!(&payload[frame.len()..], &[0u8; 20 - MIN_FRAME_LEN]);
    }

    #[test]
    fn tx_drops_frames_on_guest_memory_errors_without_panicking() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: u64::MAX - 5,
                len: (VirtioNetHeader::SIZE + MIN_FRAME_LEN) as u32,
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

        let mut backend = TestBackend::default();
        let irq = dev.process_tx(&mut mem, &mut backend).unwrap();
        assert!(irq);
        assert!(backend.frames.is_empty());
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 1);
    }

    #[test]
    fn rx_drops_frames_when_descriptor_addresses_invalid() {
        let mut mem = DenseMemory::new(0x8000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: u64::MAX - 5,
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
                addr: 0x500,
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

        let frame = [0x11u8; MIN_FRAME_LEN];
        let irq = dev.inject_rx_frame(&mut mem, &frame).unwrap();
        assert!(!irq);
        assert_eq!(dev.take_isr(), 0x0);
        assert_eq!(mem.read_u16_le(used + 2).unwrap(), 0);
    }
}
