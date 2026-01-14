use aero_devices_gpu::{
    irq_bits, mmio, ring_control, AeroGpuDeviceConfig, AeroGpuExecutorConfig,
    AeroGpuFenceCompletionMode, AeroGpuPciDevice, ImmediateAeroGpuBackend,
};
use aero_devices_gpu::ring::{
    AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC,
    FENCE_PAGE_COMPLETED_FENCE_OFFSET, FENCE_PAGE_MAGIC_OFFSET, RING_ABI_VERSION_OFFSET,
    RING_ENTRY_COUNT_OFFSET, RING_ENTRY_STRIDE_BYTES_OFFSET, RING_FLAGS_OFFSET, RING_HEAD_OFFSET,
    RING_MAGIC_OFFSET, RING_SIZE_BYTES_OFFSET, RING_TAIL_OFFSET, SUBMIT_DESC_FLAGS_OFFSET,
    SUBMIT_DESC_SIGNAL_FENCE_OFFSET, SUBMIT_DESC_SIZE_BYTES_OFFSET,
};
use aero_protocol::aerogpu::aerogpu_pci::{AEROGPU_ABI_VERSION_U32, AEROGPU_MMIO_MAGIC};
use aero_devices::pci::PciDevice;
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
    assert_eq!(dev.read(mmio::ABI_VERSION, 4) as u32, AEROGPU_ABI_VERSION_U32);
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
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES);
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    let fence_gpa = 0x3000u64;

    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa as u64);
    dev.write(mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u64);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa as u64);
    dev.write(mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u64);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Enable fence IRQ so the completion raises an interrupt.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Trigger processing and tick the device to execute deferred DMA.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.read(mmio::COMPLETED_FENCE_LO, 4) as u32, 42);
    assert_ne!(dev.read(mmio::IRQ_STATUS, 4) as u32 & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), AEROGPU_FENCE_PAGE_MAGIC);
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
    mem.write_u32(desc0_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES);
    mem.write_u32(desc0_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc0_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1);

    let desc1_gpa = desc0_gpa + u64::from(entry_stride);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc1_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 2);

    let desc2_gpa = desc1_gpa + u64::from(entry_stride);
    mem.write_u32(desc2_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES);
    mem.write_u32(desc2_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc2_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 3);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa as u64);
    dev.write(mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u64);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa as u64);
    dev.write(mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u64);
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
