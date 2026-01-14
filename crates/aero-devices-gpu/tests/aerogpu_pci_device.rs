use aero_devices::pci::PciDevice;
use aero_devices_gpu::cmd::{
    CMD_STREAM_ABI_VERSION_OFFSET, CMD_STREAM_FLAGS_OFFSET, CMD_STREAM_HEADER_SIZE_BYTES,
    CMD_STREAM_MAGIC_OFFSET, CMD_STREAM_RESERVED0_OFFSET, CMD_STREAM_RESERVED1_OFFSET,
    CMD_STREAM_SIZE_BYTES_OFFSET,
};
use aero_devices_gpu::executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use aero_devices_gpu::pci::AeroGpuDeviceConfig;
use aero_devices_gpu::regs::{
    irq_bits, mmio, ring_control, AeroGpuFormat, AerogpuErrorCode, AEROGPU_MMIO_MAGIC,
    FEATURE_ERROR_INFO, FEATURE_VBLANK,
};
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
use aero_io_snapshot::io::state::{
    codec::Encoder, IoSnapshot, SnapshotError, SnapshotReader, SnapshotVersion, SnapshotWriter,
};
use aero_protocol::aerogpu::aerogpu_cmd::{AEROGPU_CMD_STREAM_MAGIC, AEROGPU_PRESENT_FLAG_VSYNC};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use memory::{MemoryBus, MmioHandler};
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

#[derive(Clone, Debug, Default)]
struct NoDmaMemory;

impl MemoryBus for NoDmaMemory {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected physical read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected physical write");
    }
}

fn new_test_device(cfg: AeroGpuDeviceConfig) -> AeroGpuPciDevice {
    let mut dev = AeroGpuPciDevice::new(cfg);
    // Enable PCI MMIO decode + bus mastering so MMIO and DMA paths behave like a real enumerated
    // device (guests must set COMMAND.MEM/BME before touching BARs).
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev
}

#[test]
fn pci_wrapper_gates_aerogpu_mmio_on_pci_command_mem_bit() {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());

    // With COMMAND.MEM clear, reads float high and writes are ignored.
    assert_eq!(dev.read(mmio::MAGIC, 4) as u32, u32::MAX);
    assert_eq!(dev.read(mmio::MAGIC, 1), 0xFF);
    assert_eq!(dev.read(mmio::MAGIC, 2), 0xFFFF);
    assert_eq!(dev.read(mmio::MAGIC, 8), u64::MAX);
    dev.write(mmio::RING_GPA_LO, 4, 0xdead_beef);
    assert_eq!(dev.regs.ring_gpa, 0);

    // Enable MMIO decoding and verify the device responds.
    dev.config_mut().set_command(1 << 1);
    assert_eq!(dev.read(mmio::MAGIC, 4) as u32, AEROGPU_MMIO_MAGIC);
}

#[test]
fn pci_wrapper_ignores_doorbell_writes_while_mmio_decode_is_disabled() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Minimal ring: one empty submission signaling fence 42.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Disable MMIO decode but keep bus mastering enabled.
    dev.config_mut().set_command(1 << 2);

    // Doorbell write must be ignored while COMMAND.MEM is clear.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);
    assert_eq!(dev.regs.stats.doorbells, 0);

    // Re-enable MMIO decode: the ignored doorbell must not be "replayed" on tick.
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);
    assert_eq!(dev.regs.stats.doorbells, 0);

    // Writing a doorbell with MMIO decode enabled should now process and advance the fence.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(dev.regs.stats.doorbells, 1);
}

#[test]
fn pci_reset_preserves_vblank_feature_gating_from_device_config() {
    // If the device model is configured without a vblank clock, it must not advertise FEATURE_VBLANK
    // (otherwise guests may wait forever for vblank edges that will never arrive).
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg);
    assert_eq!(dev.regs.features & FEATURE_VBLANK, 0);

    // After a device reset, the config-derived feature gating must still apply.
    dev.reset();
    assert_eq!(dev.regs.features & FEATURE_VBLANK, 0);
    assert_eq!(dev.regs.scanout0_vblank_period_ns, 0);
}

#[test]
fn pci_reset_preserves_configured_vblank_period() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg);
    assert_ne!(dev.regs.features & FEATURE_VBLANK, 0);
    assert_eq!(dev.regs.scanout0_vblank_period_ns, 100_000_000);

    dev.reset();
    assert_ne!(dev.regs.features & FEATURE_VBLANK, 0);
    assert_eq!(dev.regs.scanout0_vblank_period_ns, 100_000_000);
}

#[test]
fn pci_wrapper_gates_aerogpu_dma_on_pci_command_bme_bit() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());

    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_mut().set_command(1 << 1);

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // With COMMAND.BME clear, DMA must not run.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // Once bus mastering is enabled, the previously queued doorbell should process.
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
}

#[test]
fn pci_wrapper_gates_scanout_and_cursor_readback_on_pci_command_bme_bit() {
    let mut mem = NoDmaMemory;
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());

    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_mut().set_command(1 << 1);

    // Program a valid scanout configuration. Even with a valid config, readback must be gated on
    // COMMAND.BME to avoid performing DMA reads when bus mastering is disabled.
    dev.regs.scanout0.enable = true;
    dev.regs.scanout0.width = 1;
    dev.regs.scanout0.height = 1;
    dev.regs.scanout0.pitch_bytes = 4;
    dev.regs.scanout0.fb_gpa = 0x1000;
    dev.regs.scanout0.format = AeroGpuFormat::R8G8B8A8Unorm;

    assert!(
        dev.read_scanout0_rgba(&mut mem).is_none(),
        "scanout readback must be gated on COMMAND.BME"
    );

    dev.regs.cursor.enable = true;
    dev.regs.cursor.width = 1;
    dev.regs.cursor.height = 1;
    dev.regs.cursor.pitch_bytes = 4;
    dev.regs.cursor.fb_gpa = 0x2000;
    dev.regs.cursor.format = AeroGpuFormat::R8G8B8A8Unorm;

    assert!(
        dev.read_cursor_rgba(&mut mem).is_none(),
        "cursor readback must be gated on COMMAND.BME"
    );
}

#[test]
fn ring_reset_clears_pending_doorbell_even_when_dma_is_disabled() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_mut().set_command(1 << 1);

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Queue a doorbell while DMA is disabled.
    dev.write(mmio::DOORBELL, 4, 1);

    // Now reset the ring while DMA is still disabled, but keep it enabled afterward.
    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::RESET | ring_control::ENABLE) as u64,
    );

    // Tick once with DMA disabled: this should not process the doorbell.
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    // Enable bus mastering and tick again. If the reset did not clear the pending doorbell, the
    // old submission would still complete here.
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
    assert_eq!(
        dev.regs.completed_fence, 0,
        "ring reset must drop any pending doorbell notification"
    );
}

#[test]
fn ring_reset_dma_is_deferred_until_bus_mastering_is_enabled() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_mut().set_command(1 << 1);

    // Ring header: put head behind tail so we can observe the reset DMA synchronization.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 1);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 3);

    let fence_gpa = 0x3000u64;
    // Dirty the fence page so we can ensure the reset overwrites once DMA is enabled.
    mem.write_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET, 0xDEAD_BEEF);
    mem.write_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET, 999);

    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Request a ring reset while DMA is disabled.
    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::RESET | ring_control::ENABLE) as u64,
    );

    // Tick once with COMMAND.BME clear: DMA must not run yet.
    dev.tick(&mut mem, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        0xDEAD_BEEF
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        999
    );

    // Enable bus mastering: the pending reset DMA should complete.
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
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
fn ring_reset_dma_overflow_records_oob_error_without_touching_guest_memory() {
    let mut mem = NoDmaMemory;
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default());
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    // Choose a ring GPA that will overflow when adding the ring tail offset.
    let ring_gpa = u64::MAX - (RING_TAIL_OFFSET - 1);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, 0x1000);

    // Trigger a ring reset. The DMA portion is processed by `tick` and must not wrap addresses.
    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::RESET | ring_control::ENABLE) as u64,
    );

    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.error_code, AerogpuErrorCode::Oob as u32);
    assert_eq!(dev.regs.error_fence, 0);
    assert_eq!(dev.regs.error_count, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn pci_wrapper_gates_aerogpu_intx_on_pci_command_intx_disable_bit() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Minimal ring submission that signals a fence and raises IRQ.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert!(dev.irq_level());

    // INTX_DISABLE suppresses the external interrupt line, but does not clear internal state.
    dev.config_mut()
        .set_command((1 << 1) | (1 << 2) | (1 << 10));
    assert!(!dev.irq_level());

    dev.config_mut().set_command((1 << 1) | (1 << 2));
    assert!(dev.irq_level());
}

#[test]
fn scanout_disable_stops_vblank_and_clears_pending_irq() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(cfg);

    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK as u64);

    let t0 = 0u64;
    let period_ns = 100_000_000u64; // 10 Hz
    dev.tick(&mut mem, t0);
    dev.tick(&mut mem, t0 + period_ns);

    let seq_before_disable = dev.regs.scanout0_vblank_seq;
    assert_ne!(seq_before_disable, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());

    dev.write(mmio::SCANOUT0_ENABLE, 4, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    dev.tick(&mut mem, t0 + 2 * period_ns);
    assert_eq!(dev.regs.scanout0_vblank_seq, seq_before_disable);

    // Re-enable scanout and tick before the next period: should not generate a "stale" vblank.
    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);
    dev.tick(&mut mem, t0 + 2 * period_ns + period_ns / 2);
    assert_eq!(dev.regs.scanout0_vblank_seq, seq_before_disable);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    dev.tick(&mut mem, t0 + 3 * period_ns + period_ns / 2);
    assert!(dev.regs.scanout0_vblank_seq > seq_before_disable);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

#[test]
fn enabling_vblank_irq_does_not_latch_stale_irq_from_catchup_ticks() {
    #[derive(Clone, Debug, Default)]
    struct NoDmaMemory;

    impl MemoryBus for NoDmaMemory {
        fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
            panic!("unexpected DMA read while bus mastering disabled");
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
            panic!("unexpected DMA write while bus mastering disabled");
        }
    }

    let mut mem = NoDmaMemory;
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(60),
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg);

    // Enable MMIO decoding but keep bus mastering disabled so tick cannot DMA into our dummy bus.
    dev.config_mut().set_command(1 << 1);
    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);

    let period_ns = u64::from(dev.regs.scanout0_vblank_period_ns);
    assert_ne!(period_ns, 0, "test requires vblank pacing to be enabled");

    // Establish a vblank schedule, then simulate a long host stall where we do not tick for a
    // while. This means the next vblank deadline is in the past when we later enable IRQs.
    dev.tick(&mut mem, 0);

    // Enable vblank IRQ delivery while the device is behind on its vblank scheduler. The device
    // must catch up its counters without immediately latching an interrupt for vblanks that
    // occurred while IRQ delivery was disabled.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK as u64);
    dev.tick(&mut mem, period_ns * 3);

    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(
        dev.regs.scanout0_vblank_seq > 0,
        "vblank counters must advance even when IRQ delivery was disabled"
    );

    // The next vblank edge after the enable should latch the IRQ status bit.
    dev.tick(&mut mem, period_ns * 4);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
}

#[test]
fn vsynced_present_does_not_complete_on_elapsed_vblank_before_submission() {
    // Regression test for subtle ordering: if the device needs to "catch up" its vblank clock, it
    // must do so *before* processing doorbells. Otherwise a vsynced PRESENT submitted just after a
    // vblank edge could complete on that already-elapsed edge.
    let mut mem = VecMemory::new(0x20_000);
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(100),
        ..Default::default()
    };
    let mut dev = new_test_device(cfg);

    let period_ns = u64::from(dev.regs.scanout0_vblank_period_ns);
    assert!(
        period_ns > 1,
        "test requires a non-trivial vblank period (got {period_ns})"
    );

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let cmd_gpa = 0x2000u64;
    let fence_gpa = 0x3000u64;
    let signal_fence = 1u64;

    // Command stream containing a vsynced PRESENT.
    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    mem.write_physical(cmd_gpa, &cmd_stream);

    // Ring layout in guest memory (one PRESENT submission).
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET,
        cmd_stream.len() as u32,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, signal_fence);

    // Hook up registers.
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);
    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);

    // Establish vblank schedule and tick to just before the first vblank edge.
    dev.tick(&mut mem, 0);
    dev.tick(&mut mem, period_ns - 1);

    // Jump past the vblank edge (without ticking at the exact edge), submit the doorbell, and tick
    // at the current time. The first vblank edge is already elapsed at the time of submission, so
    // the fence must not complete until the *next* vblank edge.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, period_ns + 1);

    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // Still before the next edge: fence remains pending.
    dev.tick(&mut mem, period_ns * 2 - 1);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // At the next vblank, the vsync fence becomes eligible and completes.
    dev.tick(&mut mem, period_ns * 2);
    assert_eq!(dev.regs.completed_fence, signal_fence);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        signal_fence
    );
}

#[test]
fn error_mmio_regs_latch_and_survive_irq_ack() {
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let features = (dev.read(mmio::FEATURES_HI, 4) << 32) | dev.read(mmio::FEATURES_LO, 4);
    assert_ne!(features & FEATURE_ERROR_INFO, 0);

    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::ERROR as u64);

    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::None as u32
    );
    assert_eq!(dev.read(mmio::ERROR_FENCE_LO, 4), 0);
    assert_eq!(dev.read(mmio::ERROR_FENCE_HI, 4), 0);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4), 0);

    // Use a fence value > u32::MAX to ensure ERROR_FENCE_LO/HI preserve full 64-bit fences.
    let fence = 0x1_0000_0000u64 + 42;
    dev.regs.record_error(AerogpuErrorCode::Backend, fence);
    dev.tick(&mut mem, 0);

    assert!(dev.irq_level());
    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::Backend as u32
    );
    let error_fence = (dev.read(mmio::ERROR_FENCE_LO, 4) as u64)
        | ((dev.read(mmio::ERROR_FENCE_HI, 4) as u64) << 32);
    assert_eq!(error_fence, fence);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4) as u32, 1);

    // IRQ_ACK clears only the status bit; the error payload remains latched.
    dev.write(mmio::IRQ_ACK, 4, irq_bits::ERROR as u64);
    assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(!dev.irq_level());

    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::Backend as u32
    );
    let error_fence_after_ack = (dev.read(mmio::ERROR_FENCE_LO, 4) as u64)
        | ((dev.read(mmio::ERROR_FENCE_HI, 4) as u64) << 32);
    assert_eq!(error_fence_after_ack, fence);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4) as u32, 1);

    // Ring reset is a recovery point: it clears any previously latched error payload.
    dev.write(mmio::RING_CONTROL, 4, ring_control::RESET as u64);
    dev.tick(&mut mem, 0);
    assert_eq!(
        dev.read(mmio::ERROR_CODE, 4) as u32,
        AerogpuErrorCode::None as u32
    );
    assert_eq!(dev.read(mmio::ERROR_FENCE_LO, 4), 0);
    assert_eq!(dev.read(mmio::ERROR_FENCE_HI, 4), 0);
    assert_eq!(dev.read(mmio::ERROR_COUNT, 4), 0);
}

#[test]
fn drain_pending_submissions_and_complete_fence_with_external_backend() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
    };

    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(cfg);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    // Minimal command stream: header only (no packets).
    let cmd_gpa = 0x2000u64;
    let cmd_size_bytes = CMD_STREAM_HEADER_SIZE_BYTES;
    let mut stream = vec![0u8; cmd_size_bytes as usize];
    let u32_size = core::mem::size_of::<u32>();
    let magic_range =
        (CMD_STREAM_MAGIC_OFFSET as usize)..(CMD_STREAM_MAGIC_OFFSET as usize + u32_size);
    stream[magic_range].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    let abi_range = (CMD_STREAM_ABI_VERSION_OFFSET as usize)
        ..(CMD_STREAM_ABI_VERSION_OFFSET as usize + u32_size);
    stream[abi_range].copy_from_slice(&dev.regs.abi_version.to_le_bytes());
    let size_range =
        (CMD_STREAM_SIZE_BYTES_OFFSET as usize)..(CMD_STREAM_SIZE_BYTES_OFFSET as usize + u32_size);
    stream[size_range].copy_from_slice(&cmd_size_bytes.to_le_bytes());
    let flags_range =
        (CMD_STREAM_FLAGS_OFFSET as usize)..(CMD_STREAM_FLAGS_OFFSET as usize + u32_size);
    stream[flags_range].copy_from_slice(&0u32.to_le_bytes()); // flags
    let reserved0_range =
        (CMD_STREAM_RESERVED0_OFFSET as usize)..(CMD_STREAM_RESERVED0_OFFSET as usize + u32_size);
    stream[reserved0_range].copy_from_slice(&0u32.to_le_bytes()); // reserved0
    let reserved1_range =
        (CMD_STREAM_RESERVED1_OFFSET as usize)..(CMD_STREAM_RESERVED1_OFFSET as usize + u32_size);
    stream[reserved1_range].copy_from_slice(&0u32.to_le_bytes()); // reserved1
    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 7);
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 9);
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0);
    let fence = u64::from(u32::MAX) + 1;
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence);

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Doorbell: submission becomes in-flight, but fence does not complete without an external
    // completion.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);

    let subs = dev.drain_pending_submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.signal_fence, fence);
    assert_eq!(sub.context_id, 7);
    assert_eq!(sub.engine_id, 9);
    assert_eq!(sub.flags, 0);
    assert_eq!(sub.cmd_stream, stream);
    assert!(sub.alloc_table.is_none());

    assert!(dev.drain_pending_submissions().is_empty());

    // External executor completes the fence.
    dev.complete_fence(&mut mem, fence);
    assert_eq!(dev.regs.completed_fence, fence);
    assert!(dev.regs.completed_fence > u64::from(u32::MAX));
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        fence
    );
}

#[test]
fn snapshot_roundtrip_preserves_pending_submissions_for_external_backend() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
    };

    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(cfg.clone());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    // Minimal command stream: header only (no packets).
    let cmd_gpa = 0x2000u64;
    let cmd_size_bytes = CMD_STREAM_HEADER_SIZE_BYTES;
    let mut stream = vec![0u8; cmd_size_bytes as usize];
    let u32_size = core::mem::size_of::<u32>();
    let magic_range =
        (CMD_STREAM_MAGIC_OFFSET as usize)..(CMD_STREAM_MAGIC_OFFSET as usize + u32_size);
    stream[magic_range].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    let abi_range = (CMD_STREAM_ABI_VERSION_OFFSET as usize)
        ..(CMD_STREAM_ABI_VERSION_OFFSET as usize + u32_size);
    stream[abi_range].copy_from_slice(&dev.regs.abi_version.to_le_bytes());
    let size_range =
        (CMD_STREAM_SIZE_BYTES_OFFSET as usize)..(CMD_STREAM_SIZE_BYTES_OFFSET as usize + u32_size);
    stream[size_range].copy_from_slice(&cmd_size_bytes.to_le_bytes());
    let flags_range =
        (CMD_STREAM_FLAGS_OFFSET as usize)..(CMD_STREAM_FLAGS_OFFSET as usize + u32_size);
    stream[flags_range].copy_from_slice(&0u32.to_le_bytes()); // flags
    let reserved0_range =
        (CMD_STREAM_RESERVED0_OFFSET as usize)..(CMD_STREAM_RESERVED0_OFFSET as usize + u32_size);
    stream[reserved0_range].copy_from_slice(&0u32.to_le_bytes()); // reserved0
    let reserved1_range =
        (CMD_STREAM_RESERVED1_OFFSET as usize)..(CMD_STREAM_RESERVED1_OFFSET as usize + u32_size);
    stream[reserved1_range].copy_from_slice(&0u32.to_le_bytes()); // reserved1
    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 7);
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 9);
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0);
    let fence = 42u64;
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Doorbell queues the decoded submission for external execution, but does not complete the
    // fence without an external completion callback.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    let snap = dev.save_state();

    let mut restored = new_test_device(cfg);
    restored.load_state(&snap).unwrap();

    let subs = restored.drain_pending_submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.signal_fence, fence);
    assert_eq!(sub.context_id, 7);
    assert_eq!(sub.engine_id, 9);
    assert_eq!(sub.flags, 0);
    assert_eq!(sub.cmd_stream, stream);
    assert!(sub.alloc_table.is_none());
}

#[test]
fn snapshot_roundtrip_preserves_pending_doorbell_until_dma_is_enabled() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x20_000);

    let mut dev = AeroGpuPciDevice::new(cfg.clone());
    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_mut().set_command(1 << 1);

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Queue a doorbell while DMA is disabled; it must remain pending.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);

    let snap = dev.save_state();

    // Restore into a device with DMA enabled. The pending doorbell should still process.
    let mut restored = AeroGpuPciDevice::new(cfg);
    restored.config_mut().set_command((1 << 1) | (1 << 2));
    restored.load_state(&snap).unwrap();

    restored.tick(&mut mem, 0);
    assert_eq!(restored.regs.completed_fence, 42);
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
fn snapshot_roundtrip_preserves_pending_ring_reset_dma_until_dma_is_enabled() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x20_000);

    let mut dev = AeroGpuPciDevice::new(cfg.clone());
    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_mut().set_command(1 << 1);

    // Ring header: put head behind tail so we can observe the reset DMA synchronization.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 1);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 3);

    let fence_gpa = 0x3000u64;
    // Dirty the fence page so we can ensure the reset overwrites once DMA is enabled.
    mem.write_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET, 0xDEAD_BEEF);
    mem.write_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET, 999);

    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);

    // Request a ring reset while DMA is disabled.
    dev.write(
        mmio::RING_CONTROL,
        4,
        (ring_control::RESET | ring_control::ENABLE) as u64,
    );

    // Tick once with COMMAND.BME clear: DMA must not run yet.
    dev.tick(&mut mem, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        0xDEAD_BEEF
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        999
    );

    let snap = dev.save_state();

    // Restore into a device with DMA enabled: the pending reset DMA should complete.
    let mut restored = AeroGpuPciDevice::new(cfg);
    restored.config_mut().set_command((1 << 1) | (1 << 2));
    restored.load_state(&snap).unwrap();

    restored.tick(&mut mem, 0);
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
fn complete_fence_is_deferred_until_bus_mastering_is_enabled() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
    };
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(cfg);

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    let fence = 42u64;
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Doorbell queues the decoded submission, but does not complete the fence without an external
    // completion.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);

    let subs = dev.drain_pending_submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].signal_fence, fence);

    // Disable bus mastering and complete the fence. The completion should be queued and applied
    // once COMMAND.BME is re-enabled.
    dev.config_mut().set_command(1 << 1);
    dev.complete_fence(&mut mem, fence);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, fence);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        fence
    );
}

#[test]
fn snapshot_roundtrip_preserves_pending_fence_completion_until_dma_is_enabled() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
    };
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(cfg.clone());

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    let fence = 42u64;
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    // Disable bus mastering and queue a completion.
    dev.config_mut().set_command(1 << 1);
    dev.complete_fence(&mut mem, fence);

    let snap = dev.save_state();

    // Restore into a device with DMA enabled: the pending completion should be applied.
    let mut restored = AeroGpuPciDevice::new(cfg);
    restored.config_mut().set_command((1 << 1) | (1 << 2));
    restored.load_state(&snap).unwrap();
    restored.tick(&mut mem, 0);

    assert_eq!(restored.regs.completed_fence, fence);
    assert_ne!(restored.regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        fence
    );
}

#[test]
fn snapshot_roundtrip_preserves_vblank_irq_enable_pending_suppression() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(cfg.clone());

    // Start vblank scheduling by enabling scanout and ticking once.
    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);
    let t0 = 0u64;
    dev.tick(&mut mem, t0);

    let period_ns = u64::from(dev.regs.scanout0_vblank_period_ns);
    assert_ne!(period_ns, 0, "test requires vblank pacing to be enabled");

    // Enable vblank IRQ delivery but do *not* tick immediately. The PCI wrapper defers catch-up to
    // the next `tick`, using `vblank_irq_enable_pending` to ensure old vblank edges do not latch
    // an interrupt immediately on enable.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK as u64);

    let snap = dev.save_state();

    let mut restored = new_test_device(cfg);
    restored.load_state(&snap).unwrap();

    // Simulate time advancing across several vblank edges without ticking. The first tick after
    // restore should catch up counters but must not latch an IRQ while the enable is still pending.
    let enable_time = t0 + period_ns * 3;
    restored.tick(&mut mem, enable_time);
    assert_eq!(restored.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(restored.regs.scanout0_vblank_seq > 0);
    assert!(!restored.irq_level());

    // The *next* vblank edge after the enable becomes effective should latch the IRQ bit.
    restored.tick(&mut mem, enable_time + period_ns);
    assert_ne!(restored.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(restored.irq_level());
}

#[test]
fn snapshot_roundtrip_preserves_pending_scanout_and_cursor_fb_gpa_updates() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let mut dev = new_test_device(cfg.clone());

    // Start from stable scanout/cursor framebuffer addresses.
    let scanout_fb0 = 0x1111_2222_3333_4444u64;
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, scanout_fb0);
    dev.write(mmio::SCANOUT0_FB_GPA_HI, 4, scanout_fb0 >> 32);
    assert_eq!(dev.regs.scanout0.fb_gpa, scanout_fb0);

    let cursor_fb0 = 0x5555_6666_7777_8888u64;
    dev.write(mmio::CURSOR_FB_GPA_LO, 4, cursor_fb0);
    dev.write(mmio::CURSOR_FB_GPA_HI, 4, cursor_fb0 >> 32);
    assert_eq!(dev.regs.cursor.fb_gpa, cursor_fb0);

    // Begin updating each base address by writing only the LO dword.
    let scanout_fb1 = 0x9999_AAAA_BBBB_CCCDu64;
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, scanout_fb1);
    assert_eq!(
        dev.regs.scanout0.fb_gpa, scanout_fb0,
        "LO-only write must not immediately commit scanout fb_gpa"
    );

    let cursor_fb1 = 0x0123_4567_89AB_CDEFu64;
    dev.write(mmio::CURSOR_FB_GPA_LO, 4, cursor_fb1);
    assert_eq!(
        dev.regs.cursor.fb_gpa, cursor_fb0,
        "LO-only write must not immediately commit cursor fb_gpa"
    );

    // Snapshot/restore should preserve the pending LO values so the subsequent HI write commits the
    // combined 64-bit address.
    let snap = dev.save_state();

    let mut restored = new_test_device(cfg);
    restored.load_state(&snap).unwrap();

    assert_eq!(restored.regs.scanout0.fb_gpa, scanout_fb0);
    assert_eq!(restored.regs.cursor.fb_gpa, cursor_fb0);
    assert_eq!(
        restored.read(mmio::SCANOUT0_FB_GPA_LO, 4) as u32,
        scanout_fb1 as u32,
        "pending scanout LO write should remain visible after snapshot restore"
    );
    assert_eq!(
        restored.read(mmio::CURSOR_FB_GPA_LO, 4) as u32,
        cursor_fb1 as u32,
        "pending cursor LO write should remain visible after snapshot restore"
    );

    restored.write(mmio::SCANOUT0_FB_GPA_HI, 4, scanout_fb1 >> 32);
    assert_eq!(
        restored.regs.scanout0.fb_gpa, scanout_fb1,
        "HI write must commit using the pending LO value preserved across snapshot"
    );

    restored.write(mmio::CURSOR_FB_GPA_HI, 4, cursor_fb1 >> 32);
    assert_eq!(
        restored.regs.cursor.fb_gpa, cursor_fb1,
        "HI write must commit using the pending LO value preserved across snapshot"
    );
}

#[test]
fn drain_pending_submissions_returns_completed_fences_as_well() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
    };

    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(cfg);

    // Ring layout in guest memory: two no-op submissions that signal fences 1 and 2.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2);

    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    for (slot, fence) in [1u64, 2].into_iter().enumerate() {
        let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + (slot as u64) * stride;
        mem.write_u32(
            desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
            AeroGpuSubmitDesc::SIZE_BYTES,
        );
        mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
        mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0);
        mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0);
        mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0);
        mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 0);
        mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0);
        mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0);
        mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence);
    }

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    // Even if fence 1 is completed before the external backend drains submissions, the submission
    // is still surfaced: some guest drivers may reuse fence values or rely on backend side effects
    // even if the completed fence does not advance.
    dev.complete_fence(&mut mem, 1);
    assert_eq!(dev.regs.completed_fence, 1);

    let subs = dev.drain_pending_submissions();
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].signal_fence, 1);
    assert_eq!(subs[1].signal_fence, 2);

    dev.complete_fence(&mut mem, 2);
    assert_eq!(dev.regs.completed_fence, 2);
    assert!(dev.drain_pending_submissions().is_empty());
}

#[test]
fn snapshot_restore_overwrites_torn_scanout_fb_gpa_update_tracking() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let committed_fb_gpa = 0x1111_2222_3333_4444u64;

    // Snapshot source device: stable/committed scanout fb_gpa (no LO-only update in flight).
    let mut source = new_test_device(cfg.clone());
    source.write(mmio::SCANOUT0_FB_GPA_LO, 4, committed_fb_gpa & 0xffff_ffff);
    source.write(mmio::SCANOUT0_FB_GPA_HI, 4, committed_fb_gpa >> 32);
    assert_eq!(source.regs.scanout0.fb_gpa, committed_fb_gpa);

    let snap = source.save_state();

    // Target device: start with the same committed value, then begin a torn LO-only update that
    // would affect MMIO reads if the pending state leaked across restore.
    let mut dev = new_test_device(cfg);
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, committed_fb_gpa & 0xffff_ffff);
    dev.write(mmio::SCANOUT0_FB_GPA_HI, 4, committed_fb_gpa >> 32);
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, 0xDEAD_BEEF);
    assert_eq!(dev.read(mmio::SCANOUT0_FB_GPA_LO, 4) as u32, 0xDEAD_BEEF);

    // Restoring snapshot state should overwrite any in-memory torn-update tracking so MMIO reads
    // reflect the restored snapshot state rather than a stale LO write from before restore.
    dev.load_state(&snap).unwrap();

    assert_eq!(
        dev.read(mmio::SCANOUT0_FB_GPA_LO, 4) as u32,
        committed_fb_gpa as u32
    );

    // Sanity: HI reads should always match the committed snapshot value.
    assert_eq!(
        dev.read(mmio::SCANOUT0_FB_GPA_HI, 4) as u32,
        (committed_fb_gpa >> 32) as u32
    );
    assert_eq!(dev.regs.scanout0.fb_gpa, committed_fb_gpa);
}

#[test]
fn snapshot_restore_clears_torn_scanout_fb_gpa_update_tracking_for_legacy_snapshots() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let committed_fb_gpa = 0x1111_2222_3333_4444u64;

    // Start from a stable v1.2 snapshot (source device has no LO-only update in flight), then
    // re-encode it as a v1.1 snapshot. v1.1 snapshots did not record pending LO/HI update state,
    // so loading them must clear any in-flight update tracking in the target device.
    const TAG_REGS: u16 = 1;
    let mut source = new_test_device(cfg.clone());
    source.write(mmio::SCANOUT0_FB_GPA_LO, 4, committed_fb_gpa & 0xffff_ffff);
    source.write(mmio::SCANOUT0_FB_GPA_HI, 4, committed_fb_gpa >> 32);
    let snap = source.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        SnapshotVersion::new(1, 1),
    );
    writer.field_bytes(TAG_REGS, regs);
    let legacy = writer.finish();

    let mut dev = new_test_device(cfg);
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, committed_fb_gpa & 0xffff_ffff);
    dev.write(mmio::SCANOUT0_FB_GPA_HI, 4, committed_fb_gpa >> 32);
    dev.write(mmio::SCANOUT0_FB_GPA_LO, 4, 0xDEAD_BEEF);
    assert_eq!(dev.read(mmio::SCANOUT0_FB_GPA_LO, 4) as u32, 0xDEAD_BEEF);

    dev.load_state(&legacy).unwrap();

    assert_eq!(
        dev.read(mmio::SCANOUT0_FB_GPA_LO, 4) as u32,
        committed_fb_gpa as u32
    );
    assert_eq!(
        dev.read(mmio::SCANOUT0_FB_GPA_HI, 4) as u32,
        (committed_fb_gpa >> 32) as u32
    );
    assert_eq!(dev.regs.scanout0.fb_gpa, committed_fb_gpa);
}

#[test]
fn snapshot_restore_rejects_executor_state_with_too_many_pending_fences() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_EXECUTOR: u16 = 2;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // Craft a malicious executor field that declares an extreme number of pending fences.
    let exec_bytes = Encoder::new().u32(65_537).finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_EXECUTOR, exec_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("pending_fences"));
}

#[test]
fn snapshot_restore_rejects_pending_submissions_with_too_many_entries() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_PENDING_SUBMISSIONS: u16 = 9;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
            ..Default::default()
        },
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // Declared pending submission count exceeds the executor's defensive cap.
    let pending_bytes = Encoder::new().u32(65_537).finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_PENDING_SUBMISSIONS, pending_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("pending_submissions")
    );
}

#[test]
fn snapshot_restore_rejects_pending_submissions_with_cmd_stream_len_too_large() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_PENDING_SUBMISSIONS: u16 = 9;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
            ..Default::default()
        },
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // Declare a command stream length that exceeds the executor's defensive cap, without needing to
    // include any bytes.
    let pending_bytes = Encoder::new()
        .u32(1) // count
        .u32(0) // flags
        .u32(0) // context_id
        .u32(0) // engine_id
        .u64(0) // signal_fence
        .u32(u32::MAX) // cmd_stream len
        .finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_PENDING_SUBMISSIONS, pending_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("pending_submissions.cmd_stream")
    );
}

#[test]
fn snapshot_restore_rejects_pending_submissions_with_alloc_table_len_too_large() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_PENDING_SUBMISSIONS: u16 = 9;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
            ..Default::default()
        },
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // A valid (empty) command stream followed by an alloc table length that exceeds the executor's
    // defensive cap. The oversized length should be rejected before any allocation.
    let pending_bytes = Encoder::new()
        .u32(1) // count
        .u32(0) // flags
        .u32(0) // context_id
        .u32(0) // engine_id
        .u64(0) // signal_fence
        .u32(0) // cmd_stream len
        // cmd_stream bytes (empty)
        .u32(u32::MAX) // alloc_table len
        .finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_PENDING_SUBMISSIONS, pending_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("pending_submissions.alloc_table")
    );
}

#[test]
fn snapshot_restore_rejects_pending_fence_completions_with_too_many_entries() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_PENDING_FENCE_COMPLETIONS: u16 = 14;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // Declared completion count exceeds the device's defensive cap.
    let pending_bytes = Encoder::new().u32(65_537).finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_PENDING_FENCE_COMPLETIONS, pending_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("pending_fence_completions")
    );
}

#[test]
fn snapshot_restore_v1_1_accepts_legacy_pending_fence_completions_tag() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_EXECUTOR: u16 = 2;
    const TAG_PENDING_FENCE_COMPLETIONS: u16 = 14;
    const TAG_PENDING_FENCE_COMPLETIONS_LEGACY: u16 = 10;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
    };

    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(cfg.clone());

    // Ring layout in guest memory (one no-op submission that signals fence=42).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
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
    let fence = 42u64;
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence);

    let fence_gpa = 0x3000u64;
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa);
    dev.write(mmio::FENCE_GPA_HI, 4, fence_gpa >> 32);
    dev.write(mmio::RING_GPA_LO, 4, ring_gpa);
    dev.write(mmio::RING_GPA_HI, 4, ring_gpa >> 32);
    dev.write(mmio::RING_SIZE_BYTES, 4, ring_size as u64);
    dev.write(mmio::RING_CONTROL, 4, ring_control::ENABLE as u64);
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::FENCE as u64);

    // Process the submission with DMA enabled so it becomes in-flight.
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    // Disable bus mastering and queue a completion.
    dev.config_mut().set_command(1 << 1);
    dev.complete_fence(&mut mem, fence);

    // Start from the current snapshot (v1.2), but re-encode it as v1.1 with the legacy
    // pending-fence-completions tag.
    let snap = dev.save_state();
    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();

    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();
    let executor = reader
        .bytes(TAG_EXECUTOR)
        .expect("saved snapshot missing TAG_EXECUTOR")
        .to_vec();
    let pending_fence_completions = reader
        .bytes(TAG_PENDING_FENCE_COMPLETIONS)
        .expect("expected pending fence completions tag in source snapshot")
        .to_vec();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        SnapshotVersion::new(1, 1),
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_EXECUTOR, executor);
    writer.field_bytes(
        TAG_PENDING_FENCE_COMPLETIONS_LEGACY,
        pending_fence_completions,
    );
    let v1_1 = writer.finish();

    // Restore into a device with DMA enabled: the pending completion should be applied.
    let mut restored = AeroGpuPciDevice::new(cfg);
    restored.config_mut().set_command((1 << 1) | (1 << 2));
    restored.load_state(&v1_1).unwrap();
    restored.tick(&mut mem, 0);

    assert_eq!(restored.regs.completed_fence, fence);
    assert_ne!(restored.regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        fence
    );
}

#[test]
fn snapshot_restore_rejects_executor_state_with_invalid_pending_fence_kind() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_EXECUTOR: u16 = 2;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // Executor snapshot field encoding:
    // - pending_fences_count u32
    // - { fence u64, wants_irq bool, kind u8 } * pending_fences_count
    //
    // Use an invalid kind value to ensure decode is rejected.
    let exec_bytes = Encoder::new()
        .u32(1) // pending_fences len
        .u64(0) // fence
        .bool(false)
        .u8(2) // invalid kind
        .finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_EXECUTOR, exec_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("pending_fences.kind"));
}

#[test]
fn snapshot_restore_rejects_executor_state_with_invalid_in_flight_kind() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_EXECUTOR: u16 = 2;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // Executor snapshot field encoding:
    // - pending_fences_count u32
    // - in_flight_count u32
    // - { desc fields..., kind u8, completed_backend bool, vblank_ready bool } * in_flight_count
    //
    // Provide a single in-flight entry with an invalid kind value.
    let exec_bytes = Encoder::new()
        .u32(0) // pending_fences len
        .u32(1) // in_flight len
        .u32(AeroGpuSubmitDesc::SIZE_BYTES) // desc_size_bytes
        .u32(0) // flags
        .u32(0) // context_id
        .u32(0) // engine_id
        .u64(0) // cmd_gpa
        .u32(0) // cmd_size_bytes
        .u64(0) // alloc_table_gpa
        .u32(0) // alloc_table_size_bytes
        .u64(1) // signal_fence
        .u8(2) // invalid kind
        .finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_EXECUTOR, exec_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("in_flight.kind"));
}

#[test]
fn snapshot_restore_rejects_executor_state_with_duplicate_in_flight_fence() {
    // Tags from `AeroGpuPciDevice::save_state` / `load_state`.
    const TAG_REGS: u16 = 1;
    const TAG_EXECUTOR: u16 = 2;

    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };

    let dev = new_test_device(cfg.clone());
    let snap = dev.save_state();

    let reader = SnapshotReader::parse(&snap, <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID).unwrap();
    let regs = reader
        .bytes(TAG_REGS)
        .expect("saved snapshot missing TAG_REGS")
        .to_vec();

    // Two in-flight entries using the same signal_fence must be rejected.
    let exec_bytes = Encoder::new()
        .u32(0) // pending_fences len
        .u32(2) // in_flight len
        // entry 0
        .u32(AeroGpuSubmitDesc::SIZE_BYTES)
        .u32(0)
        .u32(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(1) // signal_fence
        .u8(0)
        .bool(false)
        .bool(true)
        // entry 1 (duplicate fence)
        .u32(AeroGpuSubmitDesc::SIZE_BYTES)
        .u32(0)
        .u32(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(0)
        .u32(0)
        .u64(1) // signal_fence
        .u8(0)
        .bool(false)
        .bool(true)
        .finish();

    let mut writer = SnapshotWriter::new(
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_ID,
        <AeroGpuPciDevice as IoSnapshot>::DEVICE_VERSION,
    );
    writer.field_bytes(TAG_REGS, regs);
    writer.field_bytes(TAG_EXECUTOR, exec_bytes);
    let corrupted = writer.finish();

    let mut restored = new_test_device(cfg);
    let err = restored.load_state(&corrupted).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("in_flight.duplicate_fence"));
}
