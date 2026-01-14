use aero_devices::pci::PciDevice;
use aero_devices_gpu::cmd::{
    CMD_STREAM_ABI_VERSION_OFFSET, CMD_STREAM_FLAGS_OFFSET, CMD_STREAM_HEADER_SIZE_BYTES,
    CMD_STREAM_MAGIC_OFFSET, CMD_STREAM_RESERVED0_OFFSET, CMD_STREAM_RESERVED1_OFFSET,
    CMD_STREAM_SIZE_BYTES_OFFSET,
};
use aero_devices_gpu::ring::{
    AeroGpuAllocEntry, AeroGpuSubmitDesc, AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES,
    AEROGPU_RING_MAGIC, ALLOC_TABLE_ABI_VERSION_OFFSET, ALLOC_TABLE_ENTRY_COUNT_OFFSET,
    ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET, ALLOC_TABLE_MAGIC_OFFSET, ALLOC_TABLE_RESERVED0_OFFSET,
    ALLOC_TABLE_SIZE_BYTES_OFFSET, FENCE_PAGE_COMPLETED_FENCE_OFFSET, FENCE_PAGE_MAGIC_OFFSET,
    RING_ABI_VERSION_OFFSET, RING_ENTRY_COUNT_OFFSET, RING_ENTRY_STRIDE_BYTES_OFFSET,
    RING_FLAGS_OFFSET, RING_HEAD_OFFSET, RING_MAGIC_OFFSET, RING_SIZE_BYTES_OFFSET,
    RING_TAIL_OFFSET, SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET,
    SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, SUBMIT_DESC_CMD_GPA_OFFSET,
    SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, SUBMIT_DESC_CONTEXT_ID_OFFSET, SUBMIT_DESC_ENGINE_ID_OFFSET,
    SUBMIT_DESC_FLAGS_OFFSET, SUBMIT_DESC_SIGNAL_FENCE_OFFSET, SUBMIT_DESC_SIZE_BYTES_OFFSET,
};
use aero_devices_gpu::{
    irq_bits, mmio, ring_control, AeroGpuDeviceConfig, AeroGpuExecutorConfig,
    AeroGpuFenceCompletionMode, AeroGpuPciDevice,
};
use aero_protocol::aerogpu::{aerogpu_cmd, aerogpu_pci};
use memory::{MemoryBus, MmioHandler};
use pretty_assertions::assert_eq;

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let range = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[range]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let range = self.range(paddr, buf.len());
        self.data[range].copy_from_slice(buf);
    }
}

#[test]
fn deferred_mode_drains_submissions_and_completes_fences_via_api() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
            ..Default::default()
        },
    };
    let mut dev = AeroGpuPciDevice::new(cfg);

    // Enable PCI MMIO decode + bus mastering so MMIO and DMA paths behave like a real enumerated device.
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    let mut mem = VecMemory::new(0x20_000);

    // Ring in guest memory (one entry).
    let ring_gpa = 0x1000u64;
    let ring_size_bytes = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride_bytes = AeroGpuSubmitDesc::SIZE_BYTES;

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(
        ring_gpa + RING_ABI_VERSION_OFFSET,
        aerogpu_pci::AEROGPU_ABI_VERSION_U32,
    );
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size_bytes);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(
        ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET,
        entry_stride_bytes,
    );
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0); // flags
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command stream (header only).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(
        cmd_gpa + CMD_STREAM_MAGIC_OFFSET,
        aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC,
    );
    mem.write_u32(
        cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET,
        aerogpu_pci::AEROGPU_ABI_VERSION_U32,
    );
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    // Alloc table (header only, 0 entries).
    let alloc_gpa = 0x5000u64;
    let alloc_size_bytes = AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES;
    mem.write_u32(
        alloc_gpa + ALLOC_TABLE_MAGIC_OFFSET,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );
    mem.write_u32(
        alloc_gpa + ALLOC_TABLE_ABI_VERSION_OFFSET,
        aerogpu_pci::AEROGPU_ABI_VERSION_U32,
    );
    mem.write_u32(alloc_gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET, alloc_size_bytes);
    mem.write_u32(alloc_gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET, 0); // entry_count
    mem.write_u32(
        alloc_gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET,
        AeroGpuAllocEntry::SIZE_BYTES,
    );
    mem.write_u32(alloc_gpa + ALLOC_TABLE_RESERVED0_OFFSET, 0);

    let signal_fence = 42u64;

    // Submit desc at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0x1234); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, alloc_gpa);
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET,
        alloc_size_bytes,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, signal_fence);

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size_bytes as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::DOORBELL, 4, 1);

    // Process pending doorbell (and any deferred work).
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(
        mem.read_u32(ring_gpa + RING_HEAD_OFFSET),
        1,
        "ring head should advance"
    );

    let drained = dev.drain_pending_submissions();
    assert_eq!(drained.len(), 1);
    let sub = &drained[0];
    assert_eq!(sub.context_id, 0x1234);
    assert_eq!(sub.signal_fence, signal_fence);
    assert_eq!(sub.cmd_stream.len(), cmd_size_bytes as usize);
    let u32_size = core::mem::size_of::<u32>();
    assert_eq!(
        sub.cmd_stream[CMD_STREAM_MAGIC_OFFSET as usize
            ..(CMD_STREAM_MAGIC_OFFSET as usize + u32_size)],
        aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes()
    );
    assert_eq!(
        sub.alloc_table.as_ref().unwrap().len(),
        alloc_size_bytes as usize
    );

    // Completing the fence should advance completed_fence and raise IRQ.
    dev.complete_fence(&mut mem, signal_fence);
    assert_eq!(dev.regs.completed_fence, signal_fence);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        signal_fence
    );
}
