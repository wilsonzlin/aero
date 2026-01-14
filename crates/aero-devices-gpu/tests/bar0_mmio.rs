use aero_devices::pci::PciDevice;
use aero_devices_gpu::ring::{
    AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES,
    AEROGPU_RING_MAGIC, FENCE_PAGE_COMPLETED_FENCE_OFFSET, FENCE_PAGE_MAGIC_OFFSET,
    RING_ABI_VERSION_OFFSET, RING_ENTRY_COUNT_OFFSET, RING_ENTRY_STRIDE_BYTES_OFFSET,
    RING_FLAGS_OFFSET, RING_HEAD_OFFSET, RING_MAGIC_OFFSET, RING_SIZE_BYTES_OFFSET,
    RING_TAIL_OFFSET, SUBMIT_DESC_CMD_GPA_OFFSET, SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET,
    SUBMIT_DESC_FLAGS_OFFSET, SUBMIT_DESC_SIGNAL_FENCE_OFFSET, SUBMIT_DESC_SIZE_BYTES_OFFSET,
};
use aero_devices_gpu::{
    irq_bits, mmio, ring_control, AeroGpuDeviceConfig, AeroGpuExecutorConfig,
    AeroGpuFenceCompletionMode, AeroGpuFormat, AeroGpuPciDevice, ImmediateAeroGpuBackend,
};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuErrorCode;
use aero_protocol::aerogpu::aerogpu_pci::{AEROGPU_ABI_VERSION_U32, AEROGPU_MMIO_MAGIC};
use memory::MemoryBus;
use memory::MmioHandler;

fn new_test_device(executor: AeroGpuExecutorConfig) -> AeroGpuPciDevice {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig {
        executor,
        // Disable vblank for unit tests unless explicitly needed.
        vblank_hz: None,
    });

    // Enable PCI MMIO decode + bus mastering so MMIO and DMA paths behave like a real enumerated
    // device (guests must set COMMAND.MEM/BME before touching BARs).
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev
}

#[test]
fn id_registers_read_correctly() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    assert_eq!(dev.read(mmio::MAGIC, 4) as u32, AEROGPU_MMIO_MAGIC);
    assert_eq!(
        dev.read(mmio::ABI_VERSION, 4) as u32,
        AEROGPU_ABI_VERSION_U32
    );
}

#[test]
fn doorbell_advances_completed_fence_with_immediate_backend_in_deferred_mode() {
    let mut mem = memory::Bus::new(0x20_000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    let fence_gpa = 0x3000u64;

    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Enable fence IRQ so the completion raises an interrupt.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Trigger processing and tick the device to execute deferred DMA.
    // Use a sub-dword, unaligned write to ensure `MmioHandler::write` correctly merges into the
    // aligned 32-bit doorbell register.
    dev.write(mmio::DOORBELL + 1, 1, 0xFF);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.read(mmio::COMPLETED_FENCE_LO, 4) as u32, 42);
    assert_ne!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        42
    );
}

#[test]
fn irq_status_enable_and_ack_semantics() {
    let mut mem = memory::Bus::new(0x20_000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Ring layout: two submissions signaling fences 1 and 2.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc0_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc0_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc0_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1);

    let desc1_gpa = desc0_gpa + u64::from(entry_stride);
    mem.write_u32(
        desc1_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc1_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc1_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 2);

    let desc2_gpa = desc1_gpa + u64::from(entry_stride);
    mem.write_u32(
        desc2_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc2_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc2_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 3);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // With IRQ_ENABLE=0, completion should not latch the fence interrupt bit.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.read(mmio::COMPLETED_FENCE_LO, 4) as u32, 1);
    assert_eq!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    // Enabling after the fact must not create a "stale" interrupt.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);
    assert_eq!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    // Process the second submission; IRQ should latch now that it is enabled.
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2);
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.read(mmio::COMPLETED_FENCE_LO, 4) as u32, 2);
    assert_ne!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    // IRQ_ACK clears the status bit and drops the interrupt line.
    dev.write(mmio::IRQ_ACK, 4, irq_bits::FENCE as u64);
    assert_eq!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    // Disabling the IRQ line should also clear any pending status (RW1C-style masking).
    // Re-raise the interrupt first via the third submission.
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 3);
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.read(mmio::COMPLETED_FENCE_LO, 4) as u32, 3);
    assert_ne!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);

    dev.write(mmio::IRQ_ENABLE, 4, 0);
    assert_eq!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());
}

#[test]
fn mmio_64bit_read_write_roundtrips() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    // 64-bit write to *_LO should populate both LO/HI dwords.
    let ring_gpa = 0x1122_3344_5566_7788u64;
    dev.write(mmio::RING_GPA_LO, 8, ring_gpa);
    assert_eq!(dev.regs.ring_gpa, ring_gpa);
    assert_eq!(dev.read(mmio::RING_GPA_LO, 8), ring_gpa);

    // 64-bit read should stitch LO/HI dwords together.
    dev.regs.completed_fence = 0x8877_6655_4433_2211u64;
    assert_eq!(
        dev.read(mmio::COMPLETED_FENCE_LO, 8),
        dev.regs.completed_fence
    );
}

#[test]
fn ring_control_reset_clears_completed_fence_and_syncs_head_and_fence_page() {
    let mut mem = memory::Bus::new(0x20_000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Enable fence IRQ so the completion raises an interrupt.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.read(mmio::COMPLETED_FENCE_LO, 4) as u32, 42);
    assert!(dev.irq_level());

    // Create a gap between head and tail to ensure the RESET path synchronizes head to tail.
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 3);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);

    // Dirties so we can ensure the reset overwrites.
    mem.write_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET, 0);
    mem.write_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET, 999);

    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::ENABLE | ring_control::RESET) as u64,
    );
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status, 0);
    assert!(!dev.irq_level());
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 3);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        0
    );
}

#[test]
fn ring_reset_dma_does_not_panic_on_overflowing_ring_gpa() {
    let mut mem = memory::Bus::new(0x1000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });

    // Pick a ring GPA that would overflow when adding `RING_TAIL_OFFSET`/`RING_HEAD_OFFSET`.
    let ring_gpa = u64::MAX - 16;
    dev.write(mmio::RING_GPA_LO, 8, ring_gpa);

    // Also program an overflowing fence GPA to ensure the fence-page write path is similarly
    // resilient (it should no-op without panicking).
    let fence_gpa = u64::MAX - 8;
    dev.write(mmio::FENCE_GPA_LO, 8, fence_gpa);

    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::ENABLE | ring_control::RESET) as u64,
    );

    // Should not panic.
    dev.tick(&mut mem, 0);
}

#[test]
fn ring_reset_records_oob_error_on_ring_gpa_that_wraps_u32_access() {
    let mut mem = memory::Bus::new(0x1000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });

    // Ensure the IRQ line reflects the newly-latched ERROR status bit.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::ERROR as u64);

    // Pick a ring GPA where `ring_gpa + RING_TAIL_OFFSET` is in-range but the implied 32-bit read
    // would overflow `u64` (e.g. `tail_addr = u64::MAX`).
    let ring_gpa = u64::MAX - RING_TAIL_OFFSET;
    dev.write(mmio::RING_GPA_LO, 8, ring_gpa);

    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::ENABLE | ring_control::RESET) as u64,
    );
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.error_code, AerogpuErrorCode::Oob as u32);
    assert_eq!(dev.regs.error_count, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());
}

#[test]
fn mmio_sub_dword_reads_and_writes_are_little_endian_and_merge_correctly() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    dev.write(mmio::SCANOUT0_WIDTH, 4, 0x1122_3344);

    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH, 1) as u32, 0x44);
    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH + 1, 1) as u32, 0x33);
    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH + 2, 1) as u32, 0x22);
    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH + 3, 1) as u32, 0x11);
    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH + 1, 2) as u32, 0x2233);

    // Overwrite a single byte in the middle.
    dev.write(mmio::SCANOUT0_WIDTH + 2, 1, 0xAA);
    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH, 4) as u32, 0x11AA_3344);

    // Overwrite low and high halves.
    dev.write(mmio::SCANOUT0_WIDTH, 2, 0xBEEF);
    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH, 4) as u32, 0x11AA_BEEF);

    dev.write(mmio::SCANOUT0_WIDTH + 2, 2, 0xCAFE);
    assert_eq!(dev.read(mmio::SCANOUT0_WIDTH, 4) as u32, 0xCAFE_BEEF);
}

#[test]
fn scanout_and_cursor_mmio_writes_sanitize_format_values_and_normalize_enable() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    // Boolean registers should treat any non-zero write as enabled.
    dev.write(mmio::SCANOUT0_ENABLE, 4, 2);
    assert_eq!(dev.read(mmio::SCANOUT0_ENABLE, 4) as u32, 1);

    dev.write(mmio::CURSOR_ENABLE, 4, 123);
    assert_eq!(dev.read(mmio::CURSOR_ENABLE, 4) as u32, 1);

    // Format registers should sanitize unknown values.
    dev.write(mmio::SCANOUT0_FORMAT, 4, 0xDEAD_BEEF);
    assert_eq!(
        dev.read(mmio::SCANOUT0_FORMAT, 4) as u32,
        AeroGpuFormat::Invalid as u32
    );
    assert_eq!(dev.regs.scanout0.format, AeroGpuFormat::Invalid);

    dev.write(
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u64,
    );
    assert_eq!(
        dev.read(mmio::SCANOUT0_FORMAT, 4) as u32,
        AeroGpuFormat::B8G8R8X8Unorm as u32
    );

    dev.write(mmio::CURSOR_FORMAT, 4, 0xDEAD_BEEF);
    assert_eq!(
        dev.read(mmio::CURSOR_FORMAT, 4) as u32,
        AeroGpuFormat::Invalid as u32
    );

    dev.write(mmio::CURSOR_FORMAT, 4, AeroGpuFormat::R8G8B8A8Unorm as u64);
    assert_eq!(
        dev.read(mmio::CURSOR_FORMAT, 4) as u32,
        AeroGpuFormat::R8G8B8A8Unorm as u32
    );
    assert_eq!(dev.regs.cursor.format, AeroGpuFormat::R8G8B8A8Unorm);
}

#[test]
fn doorbell_is_ignored_until_ring_is_enabled_and_requires_a_new_kick() {
    let mut mem = memory::Bus::new(0x20_000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Ring layout in guest memory (one no-op submission that signals fence=7).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 7);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);

    // Ring is not enabled yet. A doorbell should be ignored.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert!(!dev.irq_level());
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // Enabling the ring does not retroactively process the previous doorbell; a new kick is needed.
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);

    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 7);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        7
    );
}

#[test]
fn scanout_and_cursor_fb_gpa_mmio_64bit_write_roundtrips() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    let scanout_fb_gpa = 0x1122_3344_5566_7788u64;
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 8, scanout_fb_gpa);
    assert_eq!(dev.regs.scanout0.fb_gpa, scanout_fb_gpa);
    assert_eq!(dev.read(mmio::SCANOUT0_FB_GPA_LO, 8), scanout_fb_gpa);
    assert_eq!(
        dev.read(mmio::SCANOUT0_FB_GPA_LO, 4) as u32,
        scanout_fb_gpa as u32
    );
    assert_eq!(
        dev.read(mmio::SCANOUT0_FB_GPA_HI, 4) as u32,
        (scanout_fb_gpa >> 32) as u32
    );

    let cursor_fb_gpa = 0x8877_6655_4433_2211u64;
    dev.write(mmio::CURSOR_FB_GPA_LO, 8, cursor_fb_gpa);
    assert_eq!(dev.regs.cursor.fb_gpa, cursor_fb_gpa);
    assert_eq!(dev.read(mmio::CURSOR_FB_GPA_LO, 8), cursor_fb_gpa);
    assert_eq!(
        dev.read(mmio::CURSOR_FB_GPA_LO, 4) as u32,
        cursor_fb_gpa as u32
    );
    assert_eq!(
        dev.read(mmio::CURSOR_FB_GPA_HI, 4) as u32,
        (cursor_fb_gpa >> 32) as u32
    );
}

#[test]
fn scanout_and_cursor_fb_gpa_updates_are_atomic_for_readback() {
    let mut mem = memory::Bus::new(0x20_000);

    // Use the default (immediate) backend config; this test only exercises scanout/cursor readback.
    let mut dev = new_test_device(AeroGpuExecutorConfig::default());

    let scanout_fb0 = 0x5000u64;
    let scanout_fb1 = 0x6000u64;
    mem.write_physical(scanout_fb0, &[1, 2, 3, 4]);
    mem.write_physical(scanout_fb1, &[5, 6, 7, 8]);

    dev.write(mmio::SCANOUT0_WIDTH, 4, 1);
    dev.write(mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.write(mmio::SCANOUT0_PITCH_BYTES, 4, 4);
    dev.write(
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::R8G8B8A8Unorm as u64,
    );
    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);

    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, scanout_fb0);
    dev.write(mmio::SCANOUT0_FB_GPA_HI, 4, scanout_fb0 >> 32);
    assert_eq!(
        dev.read_scanout0_rgba(&mut mem).unwrap(),
        vec![1, 2, 3, 4],
        "baseline scanout readback should use fb0"
    );

    // LO-only write must not expose a torn address to readback; keep using fb0 until HI commit.
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, scanout_fb1);
    assert_eq!(
        dev.read_scanout0_rgba(&mut mem).unwrap(),
        vec![1, 2, 3, 4],
        "scanout readback must remain on fb0 after LO-only update"
    );
    dev.write(mmio::SCANOUT0_FB_GPA_HI, 4, scanout_fb1 >> 32);
    assert_eq!(
        dev.read_scanout0_rgba(&mut mem).unwrap(),
        vec![5, 6, 7, 8],
        "scanout readback should flip to fb1 after HI commit"
    );

    let cursor_fb0 = 0x7000u64;
    let cursor_fb1 = 0x8000u64;
    mem.write_physical(cursor_fb0, &[9, 10, 11, 12]);
    mem.write_physical(cursor_fb1, &[13, 14, 15, 16]);

    dev.write(mmio::CURSOR_WIDTH, 4, 1);
    dev.write(mmio::CURSOR_HEIGHT, 4, 1);
    dev.write(mmio::CURSOR_PITCH_BYTES, 4, 4);
    dev.write(mmio::CURSOR_FORMAT, 4, AeroGpuFormat::R8G8B8A8Unorm as u64);
    dev.write(mmio::CURSOR_ENABLE, 4, 1);

    dev.write(mmio::CURSOR_FB_GPA_LO, 4, cursor_fb0);
    dev.write(mmio::CURSOR_FB_GPA_HI, 4, cursor_fb0 >> 32);
    assert_eq!(
        dev.read_cursor_rgba(&mut mem).unwrap(),
        vec![9, 10, 11, 12],
        "baseline cursor readback should use fb0"
    );

    dev.write(mmio::CURSOR_FB_GPA_LO, 4, cursor_fb1);
    assert_eq!(
        dev.read_cursor_rgba(&mut mem).unwrap(),
        vec![9, 10, 11, 12],
        "cursor readback must remain on fb0 after LO-only update"
    );
    dev.write(mmio::CURSOR_FB_GPA_HI, 4, cursor_fb1 >> 32);
    assert_eq!(
        dev.read_cursor_rgba(&mut mem).unwrap(),
        vec![13, 14, 15, 16],
        "cursor readback should flip to fb1 after HI commit"
    );
}

#[test]
fn cursor_xy_mmio_roundtrips_preserve_signed_values() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    dev.write(mmio::CURSOR_X, 4, 123);
    dev.write(mmio::CURSOR_Y, 4, 456);
    assert_eq!(dev.regs.cursor.x, 123);
    assert_eq!(dev.regs.cursor.y, 456);
    assert_eq!(dev.read(mmio::CURSOR_X, 4) as u32, 123);
    assert_eq!(dev.read(mmio::CURSOR_Y, 4) as u32, 456);

    // Signed MMIO semantics: interpret written bits as i32.
    dev.write(mmio::CURSOR_X, 4, 0xFFFF_FFFF);
    dev.write(mmio::CURSOR_Y, 4, 0x8000_0000);
    assert_eq!(dev.regs.cursor.x, -1);
    assert_eq!(dev.regs.cursor.y, i32::MIN);
    assert_eq!(dev.read(mmio::CURSOR_X, 4) as u32, 0xFFFF_FFFF);
    assert_eq!(dev.read(mmio::CURSOR_Y, 4) as u32, 0x8000_0000);
}

#[test]
fn disabling_vblank_irq_clears_pending_status_bit() {
    let mut mem = memory::Bus::new(0x1000);

    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig {
        executor: AeroGpuExecutorConfig::default(),
        vblank_hz: Some(10),
    });
    // Enable PCI MMIO decode but keep DMA disabled (no bus mastering) for this test.
    dev.config_mut().set_command(1 << 1);

    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);
    dev.tick(&mut mem, 0);
    let period_ns = u64::from(dev.regs.scanout0_vblank_period_ns);
    assert_ne!(period_ns, 0);

    // Enable vblank IRQs. Semantics: the enable transition suppresses latching for one tick.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK as u64);
    dev.tick(&mut mem, period_ns);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    dev.tick(&mut mem, period_ns * 2);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());

    // Disabling the vblank IRQ line should clear any pending status bit.
    dev.write(mmio::IRQ_ENABLE, 4, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());
}

#[test]
fn error_mmio_regs_latch_and_irq_ack_clears_only_status() {
    let mut mem = memory::Bus::new(0x20_000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Ring with two malformed submissions so ERROR_COUNT increments and ERROR_FENCE reflects the
    // latest error.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2);

    let cmd0_gpa = 0x4000u64;
    // Wrong magic -> BadHeader.
    mem.write_u32(cmd0_gpa, 0);
    mem.write_u32(cmd0_gpa + 4, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(cmd0_gpa + 8, 16);
    mem.write_u32(cmd0_gpa + 12, 0);

    let cmd1_gpa = 0x5000u64;
    // Wrong ABI version (but correct magic) -> BadHeader as well.
    mem.write_u32(cmd1_gpa, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(cmd1_gpa + 4, 0);
    mem.write_u32(cmd1_gpa + 8, 16);
    mem.write_u32(cmd1_gpa + 12, 0);

    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc0_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc0_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc0_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd0_gpa);
    mem.write_u32(desc0_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 16);
    mem.write_u64(desc0_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 100);

    let desc1_gpa = desc0_gpa + u64::from(entry_stride);
    mem.write_u32(
        desc1_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc1_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc1_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd1_gpa);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 16);
    mem.write_u64(desc1_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 200);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Enable ERROR IRQ so it asserts the line.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::ERROR as u64);

    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_ne!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::ERROR, 0);
    assert!(dev.irq_level());
    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::CmdDecode as u32
    );
    assert_eq!(dev.read(mmio::ERROR_FENCE_LO, 8), 200);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4) as u32, 2);

    // Acknowledge the IRQ: this should clear the status bit but not wipe the latched payload.
    dev.write(mmio::IRQ_ACK, 4, irq_bits::ERROR as u64);
    assert_eq!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::ERROR, 0);
    assert!(!dev.irq_level());
    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::CmdDecode as u32
    );
    assert_eq!(dev.read(mmio::ERROR_FENCE_LO, 8), 200);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4) as u32, 2);
}

#[test]
fn ring_reset_clears_latched_error_payload() {
    let mut mem = memory::Bus::new(0x20_000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Ring with one malformed submission so ERROR_* registers are populated.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    let cmd_gpa = 0x4000u64;
    // Wrong magic -> CmdDecode error.
    mem.write_u32(cmd_gpa, 0);
    mem.write_u32(cmd_gpa + 4, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(cmd_gpa + 8, 16);
    mem.write_u32(cmd_gpa + 12, 0);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 16);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 123);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Enable ERROR IRQ so it asserts the line.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::ERROR as u64);

    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());
    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::CmdDecode as u32
    );
    assert_eq!(dev.read(mmio::ERROR_FENCE_LO, 8), 123);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4) as u32, 1);

    // Ring reset should clear the latched error payload so guests don't observe stale ERROR_*
    // values after recovery.
    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::ENABLE | ring_control::RESET) as u64,
    );
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(!dev.irq_level());
    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::None as u32
    );
    assert_eq!(dev.read(mmio::ERROR_FENCE_LO, 8), 0);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4) as u32, 0);
}

#[test]
fn irq_ack_clears_only_requested_bits_and_recomputes_irq_level() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    // Seed multiple pending IRQ causes, then enable them.
    dev.regs.irq_status = irq_bits::FENCE | irq_bits::ERROR;
    dev.write(
        mmio::IRQ_ENABLE,
        4,
        (irq_bits::FENCE | irq_bits::ERROR) as u64,
    );
    assert!(dev.irq_level());

    // Clearing only one bit should leave the other pending and keep IRQ asserted.
    dev.write(mmio::IRQ_ACK, 4, irq_bits::FENCE as u64);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());

    dev.write(mmio::IRQ_ACK, 4, irq_bits::ERROR as u64);
    assert_eq!(dev.regs.irq_status, 0);
    assert!(!dev.irq_level());
}

#[test]
fn irq_ack_sub_dword_writes_clear_the_correct_bit_lanes() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command(1 << 1);

    dev.regs.irq_status = irq_bits::FENCE | irq_bits::ERROR;
    // Enable both bits using sub-dword writes to ensure byte-lane merges preserve the other bytes.
    dev.write(mmio::IRQ_ENABLE, 1, 0x01);
    dev.write(mmio::IRQ_ENABLE + 3, 1, 0x80);
    assert!(dev.irq_level());

    // Clear ERROR via the high byte lane (bit 31 is byte 3, bit 7).
    dev.write(mmio::IRQ_ACK + 3, 1, 0x80);
    assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    // Clear FENCE via the low byte lane.
    dev.write(mmio::IRQ_ACK, 1, 0x01);
    assert_eq!(dev.regs.irq_status, 0);
    assert!(!dev.irq_level());
}

#[test]
fn ring_control_readback_masks_to_enable_bit_and_reset_is_write_only() {
    let mut mem = memory::Bus::new(0x1000);

    let mut dev = new_test_device(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });

    // Writing RESET should trigger the reset path but the register itself only latches ENABLE.
    dev.write(mmio::RING_CONTROL, 4, u64::from(u32::MAX));
    assert_eq!(dev.read(mmio::RING_CONTROL, 4) as u32, ring_control::ENABLE);
    assert_eq!(dev.regs.ring_control, ring_control::ENABLE);

    // Tick to drain any pending reset-side DMA work (no-op since ring/fence GPAs are unset).
    dev.tick(&mut mem, 0);

    dev.write(mmio::RING_CONTROL, 4, 0);
    assert_eq!(dev.read(mmio::RING_CONTROL, 4) as u32, 0);
    assert_eq!(dev.regs.ring_control, 0);
}
