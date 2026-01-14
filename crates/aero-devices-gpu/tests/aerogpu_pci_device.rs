use aero_devices::pci::PciDevice;
use aero_devices_gpu::executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use aero_devices_gpu::pci::AeroGpuDeviceConfig;
use aero_devices_gpu::regs::{
    irq_bits, mmio, ring_control, AerogpuErrorCode, AEROGPU_MMIO_MAGIC, FEATURE_ERROR_INFO,
    FEATURE_VBLANK,
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
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
};
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
    dev.write(mmio::RING_GPA_LO, 4, 0xdead_beef);
    assert_eq!(dev.regs.ring_gpa, 0);

    // Enable MMIO decoding and verify the device responds.
    dev.config_mut().set_command(1 << 1);
    assert_eq!(dev.read(mmio::MAGIC, 4) as u32, AEROGPU_MMIO_MAGIC);
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
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa as u64);
    dev.write(mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u64);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa as u64);
    dev.write(mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u64);
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
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa as u64);
    dev.write(mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u64);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa as u64);
    dev.write(mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u64);
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

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa as u64);
    dev.write(mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u64);
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

    let mut mem = NoDmaMemory::default();
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
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32;
    let mut stream = vec![0u8; cmd_size_bytes as usize];
    stream[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream[4..8].copy_from_slice(&dev.regs.abi_version.to_le_bytes());
    stream[8..12].copy_from_slice(&cmd_size_bytes.to_le_bytes());
    stream[12..16].copy_from_slice(&0u32.to_le_bytes()); // flags
    stream[16..20].copy_from_slice(&0u32.to_le_bytes()); // reserved0
    stream[20..24].copy_from_slice(&0u32.to_le_bytes()); // reserved1
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
    dev.write(mmio::FENCE_GPA_LO, 4, fence_gpa as u64);
    dev.write(mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u64);

    dev.write(mmio::RING_GPA_LO, 4, ring_gpa as u64);
    dev.write(mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u64);
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
