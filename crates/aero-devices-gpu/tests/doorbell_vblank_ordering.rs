use aero_devices::pci::PciDevice;
use aero_devices_gpu::pci::AeroGpuDeviceConfig;
use aero_devices_gpu::regs::{irq_bits, mmio, ring_control};
use aero_devices_gpu::ring::{
    AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES,
    AEROGPU_RING_MAGIC, FENCE_PAGE_COMPLETED_FENCE_OFFSET, FENCE_PAGE_MAGIC_OFFSET,
    RING_ABI_VERSION_OFFSET, RING_ENTRY_COUNT_OFFSET, RING_ENTRY_STRIDE_BYTES_OFFSET,
    RING_FLAGS_OFFSET, RING_HEAD_OFFSET, RING_MAGIC_OFFSET, RING_SIZE_BYTES_OFFSET,
    RING_TAIL_OFFSET, SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET,
    SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, SUBMIT_DESC_CMD_GPA_OFFSET,
    SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, SUBMIT_DESC_CONTEXT_ID_OFFSET, SUBMIT_DESC_ENGINE_ID_OFFSET,
    SUBMIT_DESC_FLAGS_OFFSET, SUBMIT_DESC_SIGNAL_FENCE_OFFSET, SUBMIT_DESC_SIZE_BYTES_OFFSET,
};
use aero_devices_gpu::AeroGpuPciDevice;
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_PRESENT_FLAG_VSYNC;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use memory::{Bus, MemoryBus, MmioHandler};

#[test]
fn vsync_present_doorbell_does_not_complete_until_next_vblank_after_submission() {
    // Use a low vblank rate so the test has clear, round-number periods.
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };

    let mut mem = Bus::new(0x10_000);
    let mut dev = AeroGpuPciDevice::new(cfg);

    // Enable PCI MMIO decode + bus mastering so MMIO and DMA paths behave like a real enumerated
    // device (guests must set COMMAND.MEM/BME before touching BARs).
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    // Enable scanout so the vblank clock runs.
    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);

    let period_ns = u64::from(dev.regs.scanout0_vblank_period_ns);
    assert_ne!(period_ns, 0, "test requires vblank pacing to be enabled");

    // Prime the vblank scheduler so `next_vblank_deadline_ns` is defined.
    dev.tick(&mut mem, 0);

    // Ring + command stream in guest memory (one submission that signals fence=1).
    let ring_gpa = 0x1000u64;
    let cmd_gpa = 0x2000u64;
    let fence_gpa = 0x3000u64;
    let signal_fence = 1u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    mem.write_physical(cmd_gpa, &cmd_stream);

    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET,
        cmd_stream.len() as u32,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, signal_fence);

    // Program the device registers the way a real guest would.
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Simulate a long host stall (no ticks) so the next vblank deadline is in the past. Then queue
    // a doorbell *before* ticking the device. The key ordering contract is that catch-up vblank
    // ticks that occurred before the submission must not complete the vsync-present fence.
    let submit_time_ns = period_ns * 3 + 1;
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, submit_time_ns);

    assert_eq!(
        dev.regs.completed_fence, 0,
        "vsync presents must not complete during DOORBELL processing"
    );
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // Advance time by less than one vblank period: still no completion.
    dev.tick(&mut mem, submit_time_ns + period_ns / 2);
    assert_eq!(dev.regs.completed_fence, 0);

    // Advance past the next vblank edge: completion should now publish.
    dev.tick(&mut mem, submit_time_ns + period_ns);
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
