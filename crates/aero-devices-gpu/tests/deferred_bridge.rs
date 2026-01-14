use aero_devices::pci::PciDevice;
use aero_devices_gpu::{
    irq_bits, mmio, ring_control, AeroGpuDeviceConfig, AeroGpuExecutorConfig,
    AeroGpuFenceCompletionMode, AeroGpuPciDevice,
};
use aero_protocol::aerogpu::{aerogpu_cmd, aerogpu_pci, aerogpu_ring};
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
    let entry_stride_bytes = aerogpu_ring::AerogpuSubmitDesc::SIZE_BYTES as u32;

    mem.write_u32(ring_gpa, aerogpu_ring::AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, aerogpu_pci::AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + 8, ring_size_bytes);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride_bytes);
    mem.write_u32(ring_gpa + 20, 0); // flags
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Command stream (header only).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = aerogpu_cmd::AerogpuCmdStreamHeader::SIZE_BYTES as u32;
    mem.write_u32(cmd_gpa, aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(cmd_gpa + 4, aerogpu_pci::AEROGPU_ABI_VERSION_U32);
    mem.write_u32(cmd_gpa + 8, cmd_size_bytes);
    mem.write_u32(cmd_gpa + 12, 0);
    mem.write_u32(cmd_gpa + 16, 0);
    mem.write_u32(cmd_gpa + 20, 0);

    // Alloc table (header only, 0 entries).
    let alloc_gpa = 0x5000u64;
    let alloc_size_bytes = aerogpu_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32;
    mem.write_u32(alloc_gpa, aerogpu_ring::AEROGPU_ALLOC_TABLE_MAGIC);
    mem.write_u32(alloc_gpa + 4, aerogpu_pci::AEROGPU_ABI_VERSION_U32);
    mem.write_u32(alloc_gpa + 8, alloc_size_bytes);
    mem.write_u32(alloc_gpa + 12, 0); // entry_count
    mem.write_u32(
        alloc_gpa + 16,
        aerogpu_ring::AerogpuAllocEntry::SIZE_BYTES as u32,
    );
    mem.write_u32(alloc_gpa + 20, 0);

    let signal_fence = 42u64;

    // Submit desc at slot 0.
    let desc_gpa = ring_gpa + aerogpu_ring::AerogpuRingHeader::SIZE_BYTES as u64;
    mem.write_u32(desc_gpa, aerogpu_ring::AerogpuSubmitDesc::SIZE_BYTES as u32);
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0x1234); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, cmd_size_bytes);
    mem.write_u64(desc_gpa + 32, alloc_gpa);
    mem.write_u32(desc_gpa + 40, alloc_size_bytes);
    mem.write_u64(desc_gpa + 48, signal_fence);

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
    assert_eq!(mem.read_u32(ring_gpa + 24), 1, "ring head should advance");

    let drained = dev.drain_pending_submissions();
    assert_eq!(drained.len(), 1);
    let sub = &drained[0];
    assert_eq!(sub.context_id, 0x1234);
    assert_eq!(sub.signal_fence, signal_fence);
    assert_eq!(sub.cmd_stream.len(), cmd_size_bytes as usize);
    assert_eq!(
        sub.cmd_stream[0..4],
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
        mem.read_u32(fence_gpa),
        aerogpu_ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(mem.read_u64(fence_gpa + 8), signal_fence);
}
