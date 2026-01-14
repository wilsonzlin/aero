use std::collections::BTreeMap;

use aero_devices_gpu::executor::{
    AeroGpuExecutor, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode,
};
use aero_devices_gpu::regs::{irq_bits, ring_control, AeroGpuRegs, AerogpuErrorCode};
use aero_devices_gpu::ring::{
    AeroGpuSubmitDesc, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC, RING_ABI_VERSION_OFFSET,
    RING_ENTRY_COUNT_OFFSET, RING_ENTRY_STRIDE_BYTES_OFFSET, RING_FLAGS_OFFSET, RING_HEAD_OFFSET,
    RING_MAGIC_OFFSET, RING_SIZE_BYTES_OFFSET, RING_TAIL_OFFSET, SUBMIT_DESC_FLAGS_OFFSET,
    SUBMIT_DESC_SIGNAL_FENCE_OFFSET, SUBMIT_DESC_SIZE_BYTES_OFFSET,
};
use memory::MemoryBus;

/// Sparse `MemoryBus` implementation that supports addresses near `u64::MAX` without allocating a
/// gigantic contiguous buffer.
///
/// Unallocated regions read back as zeroes (similar to `SparseMemory`).
#[derive(Default)]
struct SparseU64MemoryBus {
    pages: BTreeMap<u64, Box<[u8; Self::PAGE_SIZE]>>,
}

impl SparseU64MemoryBus {
    const PAGE_SIZE: usize = 4096;

    fn page_index(addr: u64) -> u64 {
        addr / Self::PAGE_SIZE as u64
    }

    fn page_offset(addr: u64) -> usize {
        (addr % Self::PAGE_SIZE as u64) as usize
    }

    fn ensure_page(&mut self, page: u64) -> &mut [u8; Self::PAGE_SIZE] {
        self.pages
            .entry(page)
            .or_insert_with(|| Box::new([0u8; Self::PAGE_SIZE]))
    }
}

impl MemoryBus for SparseU64MemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (i, slot) in buf.iter_mut().enumerate() {
            let Some(addr) = paddr.checked_add(i as u64) else {
                *slot = 0xFF;
                continue;
            };
            let page = Self::page_index(addr);
            let off = Self::page_offset(addr);
            *slot = self
                .pages
                .get(&page)
                .map(|p| p[off])
                .unwrap_or(0u8);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, byte) in buf.iter().enumerate() {
            let Some(addr) = paddr.checked_add(i as u64) else {
                continue;
            };
            let page = Self::page_index(addr);
            let off = Self::page_offset(addr);
            self.ensure_page(page)[off] = *byte;
        }
    }
}

#[test]
fn ring_descriptor_gpa_overflow_latches_oob_and_does_not_wrap_to_low_memory() {
    let mut mem = SparseU64MemoryBus::default();
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    // Pick a ring GPA that:
    // - Allows reading the 64-byte ring header without overflowing (gpa + 63 == u64::MAX), but
    // - Overflows when forming the first descriptor GPA (gpa + 64 wraps to 0 with wrapping math).
    let ring_gpa = u64::MAX - (AEROGPU_RING_HEADER_SIZE_BYTES - 1);

    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    let ring_size_bytes =
        (AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * u64::from(entry_stride)) as u32;

    // Ring header with one pending submission (head=0, tail=1).
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size_bytes);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    // If the executor incorrectly uses wrapping arithmetic, the descriptor GPA would wrap to 0 and
    // this fence would be (incorrectly) completed.
    let wrapped_desc_gpa = 0u64;
    mem.write_u32(
        wrapped_desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(wrapped_desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(wrapped_desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 99);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size_bytes;
    regs.ring_control = ring_control::ENABLE;
    regs.irq_enable = irq_bits::ERROR;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(
        mem.read_u32(ring_gpa + RING_HEAD_OFFSET),
        1,
        "expected ring head to advance to tail on overflow"
    );
    assert_eq!(
        regs.completed_fence, 0,
        "overflow path must not complete fences from wrapped descriptors"
    );
    assert_eq!(regs.error_code, AerogpuErrorCode::Oob as u32);
    assert_eq!(regs.error_fence, 0);
    assert_eq!(regs.error_count, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

