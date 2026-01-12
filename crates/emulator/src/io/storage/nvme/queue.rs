use memory::MemoryBus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubmissionQueue {
    pub id: u16,
    pub base: u64,
    pub size: u16,
    pub head: u16,
    pub tail: u16,
    pub cqid: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionQueue {
    pub base: u64,
    pub size: u16,
    pub head: u16,
    pub tail: u16,
    pub phase: bool,
    pub host_phase: bool,
}

impl CompletionQueue {
    pub fn update_head(&mut self, new_head: u16) {
        let new_head = new_head % self.size;
        if new_head < self.head {
            self.host_phase = !self.host_phase;
        }
        self.head = new_head;
    }

    pub fn is_full(&self) -> bool {
        self.tail == self.head && self.phase != self.host_phase
    }

    pub fn is_empty(&self) -> bool {
        self.tail == self.head && self.phase == self.host_phase
    }

    pub fn push(
        &mut self,
        mem: &mut dyn MemoryBus,
        dw0: u32,
        sq_head: u16,
        sqid: u16,
        cid: u16,
        status: u16,
    ) {
        let entry_addr = self.base + (self.tail as u64) * 16;
        mem.write_u32(entry_addr, dw0);
        mem.write_u32(entry_addr + 4, 0);
        let dw2 = (sq_head as u32) | ((sqid as u32) << 16);
        mem.write_u32(entry_addr + 8, dw2);
        let dw3 = (cid as u32) | ((status as u32) << 16);
        mem.write_u32(entry_addr + 12, dw3);

        self.tail += 1;
        if self.tail >= self.size {
            self.tail = 0;
            self.phase = !self.phase;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuePair {
    pub sq: SubmissionQueue,
    pub cq: CompletionQueue,
}

pub fn read_command_dwords(mem: &mut dyn MemoryBus, addr: u64) -> [u32; 16] {
    let mut dwords = [0u32; 16];
    for (idx, slot) in dwords.iter_mut().enumerate() {
        *slot = mem.read_u32(addr + (idx as u64) * 4);
    }
    dwords
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrpError {
    Invalid,
}

// DoS guard: cap per-request DMA buffers.
//
// This should match the max transfer size we advertise via Identify Controller (MDTS=10, meaning
// 2^10 * 4KiB = 4MiB max transfer).
pub(crate) const NVME_MAX_DMA_BYTES: usize = 4 * 1024 * 1024;

fn is_page_aligned(addr: u64, page_size: usize) -> bool {
    page_size != 0 && addr.is_multiple_of(page_size as u64)
}

pub fn prp_segments(
    mem: &mut dyn MemoryBus,
    prp1: u64,
    prp2: u64,
    total_len: usize,
    page_size: usize,
) -> Result<Vec<(u64, usize)>, PrpError> {
    if total_len == 0 {
        return Ok(Vec::new());
    }
    if page_size == 0 || (page_size & (page_size - 1)) != 0 {
        return Err(PrpError::Invalid);
    }

    let first_offset = (prp1 % page_size as u64) as usize;
    let first_len = total_len.min(page_size - first_offset);

    let mut remaining = total_len - first_len;
    let additional_pages = remaining.div_ceil(page_size);
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(1 + additional_pages)
        .map_err(|_| PrpError::Invalid)?;
    segments.push((prp1, first_len));
    if remaining == 0 {
        return Ok(segments);
    }

    if additional_pages == 1 {
        if !is_page_aligned(prp2, page_size) {
            return Err(PrpError::Invalid);
        }
        segments.push((prp2, remaining));
        return Ok(segments);
    }

    if !is_page_aligned(prp2, page_size) {
        return Err(PrpError::Invalid);
    }

    let entries_per_list_page = page_size / 8;
    let mut list_addr = prp2;
    let mut entry_index = 0usize;
    let mut pages_left = additional_pages;

    while pages_left > 0 {
        if entry_index >= entries_per_list_page {
            return Err(PrpError::Invalid);
        }

        let entry = mem.read_u64(list_addr + (entry_index as u64) * 8);
        let is_chain_entry = entry_index == entries_per_list_page - 1 && pages_left > 1;
        if is_chain_entry {
            if !is_page_aligned(entry, page_size) {
                return Err(PrpError::Invalid);
            }
            list_addr = entry;
            entry_index = 0;
            continue;
        }

        if !is_page_aligned(entry, page_size) {
            return Err(PrpError::Invalid);
        }

        let seg_len = remaining.min(page_size);
        segments.push((entry, seg_len));
        remaining -= seg_len;
        pages_left -= 1;
        entry_index += 1;
    }

    Ok(segments)
}

pub fn dma_read(
    mem: &mut dyn MemoryBus,
    prp1: u64,
    prp2: u64,
    total_len: usize,
    page_size: usize,
) -> Result<Vec<u8>, PrpError> {
    if total_len > NVME_MAX_DMA_BYTES {
        return Err(PrpError::Invalid);
    }
    let segments = prp_segments(mem, prp1, prp2, total_len, page_size)?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(total_len)
        .map_err(|_| PrpError::Invalid)?;
    buf.resize(total_len, 0);
    let mut offset = 0usize;
    for (addr, len) in segments {
        let end = offset.checked_add(len).ok_or(PrpError::Invalid)?;
        let dst = buf.get_mut(offset..end).ok_or(PrpError::Invalid)?;
        mem.read_physical(addr, dst);
        offset = end;
    }
    if offset != total_len {
        return Err(PrpError::Invalid);
    }
    Ok(buf)
}

pub fn dma_write(
    mem: &mut dyn MemoryBus,
    prp1: u64,
    prp2: u64,
    data: &[u8],
    page_size: usize,
) -> Result<(), PrpError> {
    if data.len() > NVME_MAX_DMA_BYTES {
        return Err(PrpError::Invalid);
    }
    let segments = prp_segments(mem, prp1, prp2, data.len(), page_size)?;
    let mut offset = 0usize;
    for (addr, len) in segments {
        let end = offset.checked_add(len).ok_or(PrpError::Invalid)?;
        let src = data.get(offset..end).ok_or(PrpError::Invalid)?;
        mem.write_physical(addr, src);
        offset = end;
    }
    if offset != data.len() {
        return Err(PrpError::Invalid);
    }
    Ok(())
}

pub fn dma_write_zeros(
    mem: &mut dyn MemoryBus,
    prp1: u64,
    prp2: u64,
    total_len: usize,
    page_size: usize,
) -> Result<(), PrpError> {
    if total_len == 0 {
        return Ok(());
    }
    if total_len > NVME_MAX_DMA_BYTES {
        return Err(PrpError::Invalid);
    }
    let segments = prp_segments(mem, prp1, prp2, total_len, page_size)?;
    const ZERO_CHUNK: [u8; 4096] = [0u8; 4096];
    for (addr, len) in segments {
        let mut remaining = len;
        let mut cur = addr;
        while remaining > 0 {
            let to_write = remaining.min(ZERO_CHUNK.len());
            mem.write_physical(cur, &ZERO_CHUNK[..to_write]);
            cur = cur.checked_add(to_write as u64).ok_or(PrpError::Invalid)?;
            remaining -= to_write;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestMem {
        data: Vec<u8>,
    }

    impl TestMem {
        fn with_len(len: usize) -> Self {
            Self {
                data: vec![0xAA; len],
            }
        }
    }

    impl MemoryBus for TestMem {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = paddr as usize;
            let end = start.saturating_add(buf.len());
            if end > self.data.len() {
                buf.fill(0);
                return;
            }
            buf.copy_from_slice(&self.data[start..end]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = paddr as usize;
            let end = start.saturating_add(buf.len());
            if end > self.data.len() {
                self.data.resize(end, 0);
            }
            self.data[start..end].copy_from_slice(buf);
        }
    }

    #[test]
    fn dma_write_zeros_does_not_allocate_page_sized_buffer() {
        // Historically `dma_write_zeros` allocated a `page_size`-sized buffer, which could be
        // driven to absurd sizes by a malicious guest (or fuzz input). This test uses an
        // intentionally huge (but power-of-two) `page_size` so any such allocation would likely
        // OOM. With the chunked implementation, this remains lightweight.
        let mut mem = TestMem::with_len(4096);
        let page_size = 1usize << 30; // 1GiB
        dma_write_zeros(&mut mem, 0, 0, 4096, page_size).unwrap();
        assert!(mem.data.iter().all(|b| *b == 0));
    }
}
