use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdPresent as ProtocolCmdPresent,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_PRESENT_FLAG_VSYNC,
};
use aero_protocol::aerogpu::aerogpu_pci::{
    AerogpuErrorCode, AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32,
};
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuRingHeader as ProtocolRingHeader, AerogpuSubmitDesc as ProtocolSubmitDesc,
};
use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_ring as ring};
use emulator::devices::aerogpu_regs::{
    irq_bits, mmio, ring_control, AEROGPU_MMIO_MAGIC, FEATURE_TRANSFER,
};
use emulator::devices::aerogpu_ring::{
    AeroGpuRingHeader, AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_FENCE_PAGE_SIZE_BYTES,
    AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC, FENCE_PAGE_COMPLETED_FENCE_OFFSET,
    FENCE_PAGE_MAGIC_OFFSET, RING_HEAD_OFFSET, RING_TAIL_OFFSET,
};
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::ImmediateAeroGpuBackend;
use emulator::gpu_worker::aerogpu_executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use emulator::io::pci::{MmioDevice, PciDevice};
use memory::MemoryBus;

const NS_PER_MS: u64 = 1_000_000;

const RING_MAGIC_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, magic) as u64;
const RING_ABI_VERSION_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, abi_version) as u64;
const RING_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, size_bytes) as u64;
const RING_ENTRY_COUNT_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, entry_count) as u64;
const RING_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolRingHeader, entry_stride_bytes) as u64;
const RING_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, flags) as u64;

const SUBMIT_DESC_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, desc_size_bytes) as u64;
const SUBMIT_DESC_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, flags) as u64;
const SUBMIT_DESC_CONTEXT_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, context_id) as u64;
const SUBMIT_DESC_ENGINE_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, engine_id) as u64;
const SUBMIT_DESC_CMD_GPA_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, cmd_gpa) as u64;
const SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, cmd_size_bytes) as u64;
const SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_gpa) as u64;
const SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_size_bytes) as u64;
const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, signal_fence) as u64;

const CMD_STREAM_MAGIC_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdStreamHeader, magic) as u64;
const CMD_STREAM_ABI_VERSION_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, abi_version) as u64;
const CMD_STREAM_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes) as u64;
const CMD_STREAM_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdStreamHeader, flags) as u64;
const CMD_STREAM_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, reserved0) as u64;
const CMD_STREAM_RESERVED1_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, reserved1) as u64;

const CMD_HDR_OPCODE_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdHdr, opcode) as u64;
const CMD_HDR_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdHdr, size_bytes) as u64;

const CMD_PRESENT_SCANOUT_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdPresent, scanout_id) as u64;
const CMD_PRESENT_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdPresent, flags) as u64;

const CMD_STREAM_HEADER_SIZE_BYTES: u64 = ProtocolCmdStreamHeader::SIZE_BYTES as u64;
const CMD_PRESENT_SIZE_BYTES: u32 = core::mem::size_of::<ProtocolCmdPresent>() as u32;

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
struct WrapDetectMemory {
    bytes: std::collections::BTreeMap<u64, u8>,
}

impl MemoryBus for WrapDetectMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        paddr
            .checked_add(buf.len() as u64)
            .expect("physical address wrap");
        for (idx, dst) in buf.iter_mut().enumerate() {
            let addr = paddr + idx as u64;
            *dst = *self.bytes.get(&addr).unwrap_or(&0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        paddr
            .checked_add(buf.len() as u64)
            .expect("physical address wrap");
        for (idx, src) in buf.iter().copied().enumerate() {
            let addr = paddr + idx as u64;
            self.bytes.insert(addr, src);
        }
    }
}

fn new_test_device(cfg: AeroGpuDeviceConfig) -> AeroGpuPciDevice {
    let mut cfg = cfg;
    // Unit tests don't need a huge VRAM backing allocation; keep it small to avoid blowing out
    // memory when the test runner executes cases in parallel.
    cfg.vram_size_bytes = 2 * 1024 * 1024;
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    // Enable PCI MMIO decode + bus mastering so MMIO and DMA paths behave like a real enumerated
    // device (guests must set COMMAND.MEM/BME before touching BARs).
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev
}

#[test]
fn pci_wrapper_gates_aerogpu_mmio_on_pci_command_mem_bit() {
    let mut mem = VecMemory::new(0x1000);
    let mut dev = AeroGpuPciDevice::new(
        AeroGpuDeviceConfig {
            vram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        },
        0,
        0,
    );

    // With COMMAND.MEM clear, reads float high and writes are ignored.
    assert_eq!(dev.mmio_read(&mut mem, mmio::MAGIC, 4), u32::MAX);
    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, 0xdead_beef);
    assert_eq!(dev.regs.ring_gpa, 0);

    // Enable MMIO decoding and verify the device responds.
    dev.config_write(0x04, 2, 1 << 1);
    assert_eq!(dev.mmio_read(&mut mem, mmio::MAGIC, 4), AEROGPU_MMIO_MAGIC);
}

#[test]
fn pci_wrapper_gates_aerogpu_dma_on_pci_command_bme_bit() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(
        AeroGpuDeviceConfig {
            vram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        },
        0,
        0,
    );

    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_write(0x04, 2, 1 << 1);

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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    // With COMMAND.BME clear, DMA must not run.
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // Once bus mastering is enabled, the earlier doorbell should process (without requiring a
    // second doorbell write).
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
}

#[test]
fn complete_fence_is_deferred_until_bus_mastering_is_enabled() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig {
        executor: AeroGpuExecutorConfig {
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
            ..Default::default()
        },
        ..Default::default()
    });

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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    // Process the doorbell to register the fence as in-flight.
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);

    // Disable bus mastering and deliver a fence completion (simulating an out-of-process backend
    // completing while DMA is not permitted). The device must queue the completion rather than
    // dropping it.
    dev.config_write(0x04, 2, 1 << 1);
    dev.complete_fence(&mut mem, 42);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // Ticking with DMA disabled must not process the queued completion.
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    // Re-enable bus mastering and tick: the queued completion should now apply.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());
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
fn doorbell_with_ring_gpa_that_wraps_u32_access_records_oob_error_without_wrapping_dma() {
    let mut mem = WrapDetectMemory::default();
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Pick a ring GPA where `ring_gpa + RING_TAIL_OFFSET` is in-range but the implied 32-bit read
    // would wrap the u64 address space (e.g. `tail_addr = u64::MAX`).
    let ring_gpa = u64::MAX - RING_TAIL_OFFSET;
    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, 0x1000);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::ERROR);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.error_code, AerogpuErrorCode::Oob as u32);
    assert_eq!(dev.regs.error_count, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());
}

#[test]
fn ring_reset_drops_pending_doorbell_while_dma_is_disabled() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(
        AeroGpuDeviceConfig {
            vram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        },
        0,
        0,
    );

    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_write(0x04, 2, 1 << 1);

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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    // Queue a doorbell while DMA is disabled.
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Now reset the ring while DMA is still disabled, but keep it enabled afterward.
    dev.mmio_write(
        &mut mem,
        mmio::RING_CONTROL,
        4,
        ring_control::RESET | ring_control::ENABLE,
    );

    // Tick once with DMA disabled: this should not process the doorbell.
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);

    // Enable bus mastering and tick again. If the reset did not clear the pending doorbell, the
    // old submission would still complete here.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.tick(&mut mem, 0);
    assert_eq!(
        dev.regs.completed_fence, 0,
        "ring reset must drop any pending doorbell notification"
    );
}

#[test]
fn ring_reset_with_ring_gpa_that_wraps_u32_access_records_oob_error_without_wrapping_dma() {
    let mut mem = WrapDetectMemory::default();
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let ring_gpa = u64::MAX - RING_TAIL_OFFSET;
    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::ERROR);

    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::RESET);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.error_code, AerogpuErrorCode::Oob as u32);
    assert_eq!(dev.regs.error_fence, 0);
    assert_eq!(dev.regs.error_count, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());
}

#[test]
fn ring_reset_dma_is_deferred_until_bus_mastering_is_enabled() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(
        AeroGpuDeviceConfig {
            vram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        },
        0,
        0,
    );

    // Enable MMIO decode but leave bus mastering disabled.
    dev.config_write(0x04, 2, 1 << 1);

    // Ring header: put head behind tail so we can observe the reset DMA synchronization.
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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 1);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 3);

    let fence_gpa = 0x3000u64;
    // Dirty the fence page so we can ensure the reset overwrites once DMA is enabled.
    mem.write_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET, 0xDEAD_BEEF);
    mem.write_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET, 999);

    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    // Request a ring reset while DMA is disabled.
    dev.mmio_write(
        &mut mem,
        mmio::RING_CONTROL,
        4,
        ring_control::RESET | ring_control::ENABLE,
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
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
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

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert!(dev.irq_level());

    // INTX_DISABLE suppresses the external interrupt line, but does not clear internal state.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2) | (1 << 10));
    assert!(!dev.irq_level());

    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    assert!(dev.irq_level());
}

#[test]
fn doorbell_updates_ring_head_fence_page_and_irq() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    assert_eq!(dev.mmio_read(&mut mem, mmio::MAGIC, 4), AEROGPU_MMIO_MAGIC);
    assert_eq!(
        dev.mmio_read(&mut mem, mmio::ABI_VERSION, 4),
        AEROGPU_ABI_VERSION_U32
    );

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 0); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + RING_HEAD_OFFSET);
    assert_eq!(head_after, 1);

    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        42
    );

    dev.mmio_write(&mut mem, mmio::IRQ_ACK, 4, irq_bits::FENCE);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());
}

#[test]
fn error_mmio_registers_are_populated_on_decode_error_irq() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0: inconsistent cmd stream (cmd_size != 0 but cmd_gpa == 0)
    // triggers a decode error, which must latch the error MMIO registers.
    let fence = 42u64;
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 4); // cmd_size_bytes (inconsistent)
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence); // signal_fence

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);

    let code = dev.mmio_read(&mut mem, mmio::ERROR_CODE, 4);
    assert_eq!(code, AerogpuErrorCode::CmdDecode as u32);

    let error_fence = (dev.mmio_read(&mut mem, mmio::ERROR_FENCE_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::ERROR_FENCE_HI, 4) as u64) << 32);
    assert_eq!(error_fence, fence);

    let count = dev.mmio_read(&mut mem, mmio::ERROR_COUNT, 4);
    assert_eq!(count, 1);
}

#[test]
fn error_mmio_registers_support_u64_fence_values() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0: inconsistent cmd stream (cmd_size != 0 but cmd_gpa == 0)
    // triggers a decode error. Use a fence > u32::MAX to ensure the error fence HI/LO registers
    // preserve the full 64-bit value.
    let fence = 0x1_0000_0000u64 + 42;
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 4); // cmd_size_bytes (inconsistent)
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence); // signal_fence

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);

    let error_fence = (dev.mmio_read(&mut mem, mmio::ERROR_FENCE_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::ERROR_FENCE_HI, 4) as u64) << 32);
    assert_eq!(error_fence, fence);
    assert!(error_fence > u64::from(u32::MAX));
}

#[test]
fn error_mmio_payload_is_sticky_across_irq_ack_and_overwritten_by_next_error() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::ERROR);

    // Submit descriptor at slot 0: inconsistent cmd stream (cmd_size != 0 but cmd_gpa == 0)
    // triggers a decode error.
    let fence0 = 42u64;
    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc0_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u64(desc0_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0); // cmd_gpa
    mem.write_u32(desc0_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 4); // cmd_size_bytes (inconsistent)
    mem.write_u64(desc0_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence0); // signal_fence

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());

    let code0 = dev.mmio_read(&mut mem, mmio::ERROR_CODE, 4);
    let fence0_read = (dev.mmio_read(&mut mem, mmio::ERROR_FENCE_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::ERROR_FENCE_HI, 4) as u64) << 32);
    let count0 = dev.mmio_read(&mut mem, mmio::ERROR_COUNT, 4);
    assert_eq!(code0, AerogpuErrorCode::CmdDecode as u32);
    assert_eq!(fence0_read, fence0);
    assert_eq!(count0, 1);

    // IRQ_ACK clears IRQ_STATUS but must not clear the latched payload.
    dev.mmio_write(&mut mem, mmio::IRQ_ACK, 4, irq_bits::ERROR);
    assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(!dev.irq_level());

    assert_eq!(dev.mmio_read(&mut mem, mmio::ERROR_CODE, 4), code0);
    let fence0_after_ack = (dev.mmio_read(&mut mem, mmio::ERROR_FENCE_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::ERROR_FENCE_HI, 4) as u64) << 32);
    assert_eq!(fence0_after_ack, fence0_read);
    assert_eq!(dev.mmio_read(&mut mem, mmio::ERROR_COUNT, 4), count0);

    // Submit descriptor at slot 1: cmd_gpa + cmd_size overflows u64 to trigger an OOB-style decode error.
    let fence1 = 43u64;
    let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_stride);
    mem.write_u32(
        desc1_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u64(desc1_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, u64::MAX - 3);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 4);
    mem.write_u64(desc1_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence1);

    // Queue the second submission.
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());

    let code1 = dev.mmio_read(&mut mem, mmio::ERROR_CODE, 4);
    let fence1_read = (dev.mmio_read(&mut mem, mmio::ERROR_FENCE_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::ERROR_FENCE_HI, 4) as u64) << 32);
    let count1 = dev.mmio_read(&mut mem, mmio::ERROR_COUNT, 4);
    assert_eq!(code1, AerogpuErrorCode::Oob as u32);
    assert_eq!(fence1_read, fence1);
    assert_eq!(count1, 2);
}

#[test]
fn ring_reset_clears_error_mmio_payload() {
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Seed a latched error payload and enable ERROR IRQ delivery.
    dev.regs.record_error(AerogpuErrorCode::Backend, 42);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::ERROR);
    assert!(dev.irq_level());

    // Ring reset is a recovery point: it clears any previously latched error payload.
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::RESET);
    dev.tick(&mut mem, 0);

    assert_eq!(
        dev.mmio_read(&mut mem, mmio::ERROR_CODE, 4),
        AerogpuErrorCode::None as u32
    );
    assert_eq!(dev.mmio_read(&mut mem, mmio::ERROR_FENCE_LO, 4), 0);
    assert_eq!(dev.mmio_read(&mut mem, mmio::ERROR_FENCE_HI, 4), 0);
    assert_eq!(dev.mmio_read(&mut mem, mmio::ERROR_COUNT, 4), 0);
    assert!(!dev.irq_level());
}

#[test]
fn mmio_reports_transfer_feature_for_abi_1_1_plus() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    assert_eq!(
        dev.mmio_read(&mut mem, mmio::ABI_VERSION, 4),
        AEROGPU_ABI_VERSION_U32
    );

    let features = (dev.mmio_read(&mut mem, mmio::FEATURES_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::FEATURES_HI, 4) as u64) << 32);

    if AEROGPU_ABI_MINOR >= 1 {
        assert_ne!(features & FEATURE_TRANSFER, 0);
        assert_eq!(AerogpuCmdOpcode::CopyBuffer as u32, 0x105);
        assert_eq!(AerogpuCmdOpcode::CopyTexture2d as u32, 0x106);
    } else {
        assert_eq!(features & FEATURE_TRANSFER, 0);
    }
}

#[test]
fn doorbell_accepts_newer_minor_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header: advertise an unknown minor version while keeping the same major.
    // The protocol versioning rules require consumers to accept forward-compatible minor
    // extensions and ignore what they don't understand.
    let newer_minor = (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1);
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, newer_minor);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 0); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.stats.malformed_submissions, 0);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
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
fn doorbell_accepts_larger_submit_desc_stride_and_size() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 128u32;

    // Ring header.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2); // tail

    // Submit descriptors at slots 0 and 1 with an extended size.
    // Slot 1 is deliberately placed at header + entry_stride to verify the device uses
    // `entry_stride_bytes` when walking the ring.
    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc0_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, 128); // desc_size_bytes
    mem.write_u64(desc0_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 41); // signal_fence

    let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_stride);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, 128); // desc_size_bytes
    mem.write_u64(desc1_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.stats.malformed_submissions, 0);
    assert_eq!(dev.regs.stats.submissions, 2);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 2);
    assert_eq!(dev.regs.completed_fence, 42);
}

#[test]
fn doorbell_rejects_unknown_major_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header: advertise an unsupported major version.
    let unsupported_major = (AEROGPU_ABI_MAJOR + 1) << 16;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, unsupported_major);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0 (should not be processed due to ABI mismatch).
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::ERROR);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());

    // Ring and fence state should remain unchanged.
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);
}

#[test]
fn ring_abi_matches_c_header() {
    assert_eq!(AEROGPU_RING_MAGIC, 0x474E_5241);
    assert_eq!(AEROGPU_ABI_VERSION_U32, 0x0001_0004);
    assert_eq!(AEROGPU_RING_HEADER_SIZE_BYTES, 64);
    assert_eq!(RING_HEAD_OFFSET, 24);
    assert_eq!(RING_TAIL_OFFSET, 28);

    assert_eq!(AEROGPU_FENCE_PAGE_MAGIC, 0x434E_4546);
    assert_eq!(AEROGPU_FENCE_PAGE_SIZE_BYTES, 56);
    assert_eq!(FENCE_PAGE_COMPLETED_FENCE_OFFSET, 8);
}

#[test]
fn ring_header_validation_checks_magic_and_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Start with a header that is valid in every way except the field under test.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 0);

    let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
    assert!(!ring.is_valid(ring_size), "wrong magic must be rejected");

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, 0);

    let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
    assert!(
        !ring.is_valid(ring_size),
        "wrong ABI version must be rejected"
    );

    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);

    let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
    assert!(ring.is_valid(ring_size));
}

#[test]
fn scanout_bgra_converts_to_rgba() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb_gpa = 0x5000u64;
    // 2x1 pixels, BGRA: (R=1,G=2,B=3,A=4), (R=10,G=20,B=30,A=40).
    mem.write_physical(fb_gpa, &[3, 2, 1, 4, 30, 20, 10, 40]);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 2);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8A8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 8);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![1, 2, 3, 4, 10, 20, 30, 40]);
}

#[test]
fn scanout_bgrx_forces_opaque_alpha() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb_gpa = 0x5000u64;
    // 2x1 pixels, BGRX: (R=1,G=2,B=3,X=0), (R=10,G=20,B=30,X=0).
    // X8 formats should force the returned alpha channel to 0xFF.
    mem.write_physical(fb_gpa, &[3, 2, 1, 0, 30, 20, 10, 0]);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 2);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 8);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![1, 2, 3, 255, 10, 20, 30, 255]);
}

#[test]
fn scanout_fb_gpa_updates_are_atomic_for_readback() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb0 = 0x5000u64;
    let fb1 = 0x6000u64;

    // 1x1 pixels, RGBA: (1,2,3,4) then (5,6,7,8).
    mem.write_physical(fb0, &[1, 2, 3, 4]);
    mem.write_physical(fb1, &[5, 6, 7, 8]);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 4);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::R8G8B8A8Unorm as u32,
    );

    // Initial framebuffer address (LO then HI).
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb0 as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb0 >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    let rgba0 = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba0, vec![1, 2, 3, 4]);

    // Begin updating `fb_gpa` by writing only the LO dword. The device must not expose a torn
    // address to the readback path; it should keep using the previous stable framebuffer address
    // until the HI dword commit.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb1 as u32);

    let rgba_after_lo = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba_after_lo, vec![1, 2, 3, 4]);

    // Commit the new address by writing HI.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb1 >> 32) as u32);

    let rgba1 = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba1, vec![5, 6, 7, 8]);
}

#[test]
fn cursor_fb_gpa_updates_are_atomic_for_readback() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb0 = 0x5000u64;
    let fb1 = 0x6000u64;

    // 1x1 pixels, RGBA: (1,2,3,4) then (5,6,7,8).
    mem.write_physical(fb0, &[1, 2, 3, 4]);
    mem.write_physical(fb1, &[5, 6, 7, 8]);

    dev.mmio_write(&mut mem, mmio::CURSOR_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_HEIGHT, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_PITCH_BYTES, 4, 4);
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FORMAT,
        4,
        AeroGpuFormat::R8G8B8A8Unorm as u32,
    );

    // Initial framebuffer address (LO then HI).
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_LO, 4, fb0 as u32);
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_HI, 4, (fb0 >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::CURSOR_ENABLE, 4, 1);

    let rgba0 = dev.read_cursor_rgba(&mut mem).unwrap();
    assert_eq!(rgba0, vec![1, 2, 3, 4]);

    // Begin updating `cursor_fb_gpa` by writing only the LO dword. The device must not expose a
    // partially-updated address to the readback path; it should keep using the previous stable
    // cursor framebuffer address until the HI dword commit.
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_LO, 4, fb1 as u32);

    let rgba_after_lo = dev.read_cursor_rgba(&mut mem).unwrap();
    assert_eq!(rgba_after_lo, vec![1, 2, 3, 4]);

    // Commit the new address by writing HI.
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_HI, 4, (fb1 >> 32) as u32);

    let rgba1 = dev.read_cursor_rgba(&mut mem).unwrap();
    assert_eq!(rgba1, vec![5, 6, 7, 8]);
}

#[test]
fn scanout_bgrx_srgb_forces_opaque_alpha() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb_gpa = 0x5000u64;
    // 2x1 pixels, BGRX: (R=240,G=128,B=16,X=0), (R=128,G=64,B=32,X=0).
    // sRGB variants should be treated identically (raw byte -> RGBA; no colorspace transform).
    mem.write_physical(fb_gpa, &[16, 128, 240, 0, 32, 64, 128, 0]);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 2);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8UnormSrgb as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 8);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![240, 128, 16, 255, 128, 64, 32, 255]);
}

#[test]
fn scanout_rgbx_forces_opaque_alpha() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb_gpa = 0x5000u64;
    // 2x1 pixels, RGBX: (R=1,G=2,B=3,X=0), (R=10,G=20,B=30,X=0).
    // X8 formats should force the returned alpha channel to 0xFF.
    mem.write_physical(fb_gpa, &[1, 2, 3, 0, 10, 20, 30, 0]);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 2);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::R8G8B8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 8);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![1, 2, 3, 255, 10, 20, 30, 255]);
}

#[test]
fn cursor_bgrx_forces_opaque_alpha() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb_gpa = 0x5000u64;
    // 1x1 pixel, BGRX: (R=240,G=128,B=16,X=0).
    mem.write_physical(fb_gpa, &[16, 128, 240, 0]);

    dev.mmio_write(&mut mem, mmio::CURSOR_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::CURSOR_PITCH_BYTES, 4, 4);
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_cursor_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![240, 128, 16, 255]);
}

#[test]
fn cursor_bgrx_srgb_forces_opaque_alpha() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    let fb_gpa = 0x5000u64;
    // 1x1 pixel, BGRX: (R=240,G=128,B=16,X=0).
    // sRGB variants should be treated identically (raw byte -> RGBA; no colorspace transform).
    mem.write_physical(fb_gpa, &[16, 128, 240, 0]);

    dev.mmio_write(&mut mem, mmio::CURSOR_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8UnormSrgb as u32,
    );
    dev.mmio_write(&mut mem, mmio::CURSOR_PITCH_BYTES, 4, 4);
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_cursor_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![240, 128, 16, 255]);
}

#[test]
fn vblank_tick_sets_irq_status() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(cfg);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK);

    let t0 = 0;
    dev.tick(&mut mem, t0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    dev.tick(&mut mem, t0 + 100 * NS_PER_MS);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

#[test]
fn enabling_vblank_irq_does_not_immediately_fire_on_catchup_ticks() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(cfg);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    // Create a vblank schedule anchored in the past without calling tick() again. This makes the
    // next vblank deadline already elapsed by the time we enable vblank interrupts.
    let past = 0;
    dev.tick(&mut mem, past);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK);

    // A catch-up tick that covers vblanks that occurred *before* enabling must not latch the vblank
    // IRQ bit (WaitForVerticalBlankEvent must wait for the next vblank after enabling).
    let now = 500 * NS_PER_MS;
    dev.tick(&mut mem, now);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    dev.tick(&mut mem, now + 100 * NS_PER_MS);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

#[test]
fn vsynced_present_fence_completes_on_vblank() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(cfg);

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32 + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(cmd_gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(
        cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET,
        dev.regs.abi_version,
    );
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = cmd_gpa + CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(
        present_gpa + CMD_HDR_OPCODE_OFFSET,
        AerogpuCmdOpcode::Present as u32,
    );
    mem.write_u32(
        present_gpa + CMD_HDR_SIZE_BYTES_OFFSET,
        CMD_PRESENT_SIZE_BYTES,
    );
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(
        present_gpa + CMD_PRESENT_FLAGS_OFFSET,
        AEROGPU_PRESENT_FLAG_VSYNC,
    );

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    ); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Consume the doorbell and queue the vsync-paced fence. No vblank has elapsed yet, so the fence
    // should remain pending.
    let t0 = 0;
    dev.tick(&mut mem, t0);

    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + RING_HEAD_OFFSET);
    assert_eq!(head_after, 1);

    dev.tick(&mut mem, t0 + 100 * NS_PER_MS);

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

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
fn vsynced_present_fence_completes_on_vblank_with_deferred_backend() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
        ..Default::default()
    };

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(cfg);
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32 + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(cmd_gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(
        cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET,
        dev.regs.abi_version,
    );
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = cmd_gpa + CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(
        present_gpa + CMD_HDR_OPCODE_OFFSET,
        AerogpuCmdOpcode::Present as u32,
    );
    mem.write_u32(
        present_gpa + CMD_HDR_SIZE_BYTES_OFFSET,
        CMD_PRESENT_SIZE_BYTES,
    );
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(
        present_gpa + CMD_PRESENT_FLAGS_OFFSET,
        AEROGPU_PRESENT_FLAG_VSYNC,
    );

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    ); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Consume the doorbell and queue the vsync-paced fence. No vblank has elapsed yet, so the fence
    // should remain pending.
    let t0 = 0;
    dev.tick(&mut mem, t0);

    // Backend completes immediately, but vsynced presents should still wait until vblank.
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + RING_HEAD_OFFSET);
    assert_eq!(head_after, 1);

    dev.tick(&mut mem, t0 + 100 * NS_PER_MS);

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

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
fn drain_pending_submissions_and_complete_fence_with_external_backend() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        vram_size_bytes: 2 * 1024 * 1024,
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
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42);

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    // Doorbell: submission becomes in-flight, but fence does not complete without an external
    // completion.
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);

    let subs = dev.drain_pending_submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.signal_fence, 42);
    assert_eq!(sub.context_id, 7);
    assert_eq!(sub.engine_id, 9);
    assert_eq!(sub.flags, 0);
    assert_eq!(sub.cmd_stream, stream);
    assert!(sub.alloc_table.is_none());

    assert!(dev.drain_pending_submissions().is_empty());

    // External executor completes the fence.
    dev.complete_fence(&mut mem, 42);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());
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
fn drain_pending_submissions_and_complete_fence_across_u32_boundary() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: None,
        vram_size_bytes: 2 * 1024 * 1024,
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
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2);

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

    // Simulate a 32-bit WDDM fence wrap (extended into a 64-bit epoch domain by the KMD):
    //   0xFFFF_FFFF -> 0x1_0000_0000
    let fence0 = u64::from(u32::MAX);
    let fence1 = u64::from(u32::MAX) + 1;

    // Submit descriptor at slot 0.
    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc0_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc0_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u32(desc0_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 7);
    mem.write_u32(desc0_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 9);
    mem.write_u64(desc0_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(
        desc0_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET,
        cmd_size_bytes,
    );
    mem.write_u64(desc0_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0);
    mem.write_u32(desc0_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0);
    mem.write_u64(desc0_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence0);

    // Submit descriptor at slot 1.
    let desc1_gpa = desc0_gpa + u64::from(entry_stride);
    mem.write_u32(
        desc1_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc1_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 7);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 9);
    mem.write_u64(desc1_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(
        desc1_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET,
        cmd_size_bytes,
    );
    mem.write_u64(desc1_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0);
    mem.write_u64(desc1_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, fence1);

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    // Doorbell: submissions become in-flight, but fences do not complete without external completions.
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);

    let subs = dev.drain_pending_submissions();
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].signal_fence, fence0);
    assert_eq!(subs[1].signal_fence, fence1);

    // External executor completes both fences (in order).
    dev.complete_fence(&mut mem, fence0);
    assert_eq!(dev.regs.completed_fence, fence0);
    dev.complete_fence(&mut mem, fence1);
    assert_eq!(dev.regs.completed_fence, fence1);
    assert!(dev.regs.completed_fence > u64::from(u32::MAX));

    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        fence1
    );
}

#[test]
fn vsynced_present_does_not_complete_on_catchup_vblank_before_submission() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(cfg);

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    // Simulate the host not calling `tick()` for a while by creating a vblank schedule anchored in
    // the past. This makes `next_vblank_ns` < `now_ns` before we submit work.
    let past = 0;
    dev.tick(&mut mem, past);

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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32 + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(cmd_gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(
        cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET,
        dev.regs.abi_version,
    );
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = cmd_gpa + CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(
        present_gpa + CMD_HDR_OPCODE_OFFSET,
        AerogpuCmdOpcode::Present as u32,
    );
    mem.write_u32(
        present_gpa + CMD_HDR_SIZE_BYTES_OFFSET,
        CMD_PRESENT_SIZE_BYTES,
    );
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(
        present_gpa + CMD_PRESENT_FLAGS_OFFSET,
        AEROGPU_PRESENT_FLAG_VSYNC,
    );

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    ); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Catch-up vblanks that happened before the submission must not complete a vsynced present.
    // `tick()` catches up vblank state before consuming the pending doorbell.
    let now = 500 * NS_PER_MS;
    dev.tick(&mut mem, now);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);

    // The next vblank after the submission should complete the fence.
    dev.tick(&mut mem, now + 100 * NS_PER_MS);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
}

#[test]
fn vsynced_present_does_not_complete_on_elapsed_vblank_before_submission() {
    // Repro case for [AEROGPU-PRESENT-PACING-01]:
    //
    // vblank period = 10ms
    // tick at t=9ms (no vblank yet)
    // submit a vsync present at t=11ms via DOORBELL without an intervening tick
    // ensure completion happens on the *next* vblank (>=20ms), not at 10ms.
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(100),
        ..Default::default()
    };

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(cfg);

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

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
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32 + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(cmd_gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(
        cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET,
        dev.regs.abi_version,
    );
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = cmd_gpa + CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(
        present_gpa + CMD_HDR_OPCODE_OFFSET,
        AerogpuCmdOpcode::Present as u32,
    );
    mem.write_u32(
        present_gpa + CMD_HDR_SIZE_BYTES_OFFSET,
        CMD_PRESENT_SIZE_BYTES,
    );
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(
        present_gpa + CMD_PRESENT_FLAGS_OFFSET,
        AEROGPU_PRESENT_FLAG_VSYNC,
    );

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    ); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    // Establish a vblank schedule such that the next vblank is at t=10ms. We'll submit a vsync
    // present at t=11ms via DOORBELL without a host tick at t=10ms; the device must catch up vblank
    // state before processing the doorbell so the present cannot complete on the already-elapsed
    // vblank edge.
    let t0 = 0u64;
    dev.tick(&mut mem, t0);
    dev.tick(&mut mem, t0 + 9 * NS_PER_MS);

    // Submit at ~t=11ms without a host tick at t=10ms. DOORBELL must catch up the vblank clock
    // before accepting new work, so the vsynced present can't complete on the already-elapsed
    // vblank edge at t=10ms.
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, t0 + 11 * NS_PER_MS);
    assert_eq!(dev.regs.completed_fence, 0);

    // The tick that processed the doorbell should have caught up the vblank edge at t=10ms (and
    // possibly more if the host clock advanced further than expected). The vsynced fence must not
    // complete on any already-elapsed vblank edges.
    let vblank_seq = u32::try_from(dev.regs.scanout0_vblank_seq).expect("vblank seq fits u32");
    assert!(
        vblank_seq >= 1,
        "expected tick to catch up vblank scheduling before processing the doorbell"
    );
    let period_ns = u64::from(dev.regs.scanout0_vblank_period_ns);
    let next_vblank = t0 + period_ns * u64::from(vblank_seq + 1);

    // No completion before the next vblank after the submission.
    dev.tick(&mut mem, next_vblank - 1);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);

    // Fence completes on the next vblank after the submission (>=20ms).
    dev.tick(&mut mem, next_vblank);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
}

#[test]
fn scanout_disable_stops_vblank_and_clears_pending_irq() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(cfg);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK);

    let t0 = 0;
    dev.tick(&mut mem, t0);
    dev.tick(&mut mem, t0 + 100 * NS_PER_MS);

    let seq_before_disable = dev.regs.scanout0_vblank_seq;
    assert_ne!(seq_before_disable, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    dev.tick(&mut mem, t0 + 200 * NS_PER_MS);
    assert_eq!(dev.regs.scanout0_vblank_seq, seq_before_disable);

    // Re-enable scanout and tick before the next period: should not generate a "stale" vblank.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.tick(&mut mem, t0 + 250 * NS_PER_MS);
    assert_eq!(dev.regs.scanout0_vblank_seq, seq_before_disable);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    dev.tick(&mut mem, t0 + 350 * NS_PER_MS);
    assert!(dev.regs.scanout0_vblank_seq > seq_before_disable);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_bits().to_le_bytes());
}

fn fnv1a32(s: &str) -> u32 {
    let mut hash = 2166136261u32;
    for b in s.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

fn read_pixel_bgra(
    mem: &mut dyn MemoryBus,
    base_gpa: u64,
    pitch_bytes: u32,
    x: u32,
    y: u32,
) -> u32 {
    let addr = base_gpa + (y as u64) * (pitch_bytes as u64) + (x as u64) * 4;
    mem.read_u32(addr)
}

#[test]
fn cmd_exec_d3d9_triangle_renders_to_guest_memory() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1); // entry_count
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0); // reserved0
                                   // entry 0
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0); // flags
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0); // reserved0
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1); // texture_handle
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1); // AEROGPU_FORMAT_B8G8R8A8_UNORM
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1); // mip_levels
    push_u32(&mut stream, 1); // array_layers
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1); // backing_alloc_id
    push_u32(&mut stream, 0); // backing_offset_bytes
    push_u64(&mut stream, 0); // reserved0

    // CREATE_BUFFER (handle 2) host-allocated.
    const VTX_STRIDE_D3D9: u32 = 20;
    const VERTS_D3D9: usize = 3;
    let vb_bytes = (VTX_STRIDE_D3D9 as usize) * VERTS_D3D9;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2); // buffer_handle
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_bytes as u64);
    push_u32(&mut stream, 0); // backing_alloc_id
    push_u32(&mut stream, 0); // backing_offset_bytes
    push_u64(&mut stream, 0); // reserved0

    // UPLOAD_RESOURCE into VB.
    let mut vb_payload = Vec::with_capacity(vb_bytes);
    // Vertex format: {x,y,z,rhw,color_argb}.
    let green_argb = 0xFF00FF00u32;
    let w = rt_width as f32;
    let h = rt_height as f32;
    for (x, y) in [
        (w * 0.25, h * 0.25),
        (w * 0.75, h * 0.25),
        (w * 0.5, h * 0.75),
    ] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.5);
        push_f32(&mut vb_payload, 1.0);
        push_u32(&mut vb_payload, green_argb);
    }
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2); // resource_handle
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0); // offset_bytes
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // SET_RENDER_TARGETS: RT0 = texture 1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1); // color_count
    push_u32(&mut stream, 0); // depth_stencil
    push_u32(&mut stream, 1); // colors[0]
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 16 + 16);
    push_u32(&mut stream, 0); // start_slot
    push_u32(&mut stream, 1); // buffer_count
    push_u32(&mut stream, 2); // binding.buffer
    push_u32(&mut stream, VTX_STRIDE_D3D9);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY TRIANGLELIST.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4); // AEROGPU_TOPOLOGY_TRIANGLELIST
    push_u32(&mut stream, 0);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3); // vertex_count
    push_u32(&mut stream, 1); // instance_count
    push_u32(&mut stream, 0); // first_vertex
    push_u32(&mut stream, 0); // first_instance

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);
    assert_eq!(mem.read_u32(fence_gpa), AEROGPU_FENCE_PAGE_MAGIC);

    let center = read_pixel_bgra(
        &mut mem,
        rt_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    );
    let corner = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 5, 5);
    assert_eq!(center & 0x00FF_FFFF, 0x0000_FF00); // green
    assert_eq!(corner & 0x00FF_FFFF, 0x00FF_0000); // red
}

#[test]
fn cmd_exec_d3d11_input_layout_triangle_renders_to_guest_memory() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1); // entry_count
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float2) + COLOR(float4).
    const VTX_STRIDE_D3D11: u32 = 24;
    let mut vb_payload = Vec::with_capacity(3 * (VTX_STRIDE_D3D11 as usize));
    for (x, y) in [(-0.5f32, -0.5f32), (0.0f32, 0.5f32), (0.5f32, -0.5f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2); // element_count
    push_u32(&mut ilay, 0);

    // POSITION
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16); // DXGI_FORMAT_R32G32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 0); // offset 0
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    // COLOR
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 8); // offset 8
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1); // texture_handle
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1); // AEROGPU_FORMAT_B8G8R8A8_UNORM
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 2) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 3) with ILAY blob.
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS: RT0 = texture 1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, VTX_STRIDE_D3D11);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY TRIANGLELIST.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 2);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 2);

    let center = read_pixel_bgra(
        &mut mem,
        rt_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    );
    let corner = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 0, 0);
    assert_eq!(center & 0x00FF_FFFF, 0x0000_FF00);
    assert_eq!(corner & 0x00FF_FFFF, 0x00FF_0000);
}

#[test]
fn cmd_exec_copy_buffer_writeback_to_guest_memory() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the destination buffer.
    let buf_size = 16u64;
    let dst_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1); // entry_count
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0); // reserved0
                                   // entry 0
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0); // flags
    push_u64(&mut alloc_table, dst_alloc_gpa);
    push_u64(&mut alloc_table, buf_size);
    push_u64(&mut alloc_table, 0); // reserved0
    mem.write_physical(alloc_table_gpa, &alloc_table);

    let src_payload: Vec<u8> = (0..buf_size as u8).map(|v| v ^ 0xA5).collect();

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_BUFFER (handle 1) host-allocated source.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, buf_size);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into source buffer.
    let upload_size_no_pad = 32 + src_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, src_payload.len() as u64);
    stream.extend_from_slice(&src_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_BUFFER (handle 2) guest-backed destination buffer.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, buf_size);
    push_u32(&mut stream, 1); // backing_alloc_id
    push_u32(&mut stream, 0); // backing_offset_bytes
    push_u64(&mut stream, 0);

    // COPY_BUFFER src -> dst with WRITEBACK_DST.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CopyBuffer as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 2); // dst_buffer
    push_u32(&mut stream, 1); // src_buffer
    push_u64(&mut stream, 0); // dst_offset_bytes
    push_u64(&mut stream, 0); // src_offset_bytes
    push_u64(&mut stream, buf_size);
    push_u32(&mut stream, cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let mut dst_buf = vec![0u8; src_payload.len()];
    mem.read_physical(dst_alloc_gpa, &mut dst_buf);
    assert_eq!(dst_buf, src_payload);
}

#[test]
fn cmd_exec_copy_texture2d_writeback_to_guest_memory() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Destination texture backing allocation (64x64 BGRA).
    let tex_width = 64u32;
    let tex_height = 64u32;
    let tex_pitch = tex_width * 4;
    let tex_bytes = (tex_pitch * tex_height) as u64;
    let dst_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1); // entry_count
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0); // flags
    push_u64(&mut alloc_table, dst_alloc_gpa);
    push_u64(&mut alloc_table, tex_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) host-allocated render target.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1); // texture_handle
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1); // AEROGPU_FORMAT_B8G8R8A8_UNORM
    push_u32(&mut stream, tex_width);
    push_u32(&mut stream, tex_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0); // row_pitch_bytes (computed by host)
    push_u32(&mut stream, 0); // backing_alloc_id
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 2) host-allocated vertex buffer.
    const VTX_STRIDE_D3D9: u32 = 20;
    const VERTS_D3D9: usize = 3;
    let vb_bytes = (VTX_STRIDE_D3D9 as usize) * VERTS_D3D9;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_bytes as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let mut vb_payload = Vec::with_capacity(vb_bytes);
    let green_argb = 0xFF00FF00u32;
    let w = tex_width as f32;
    let h = tex_height as f32;
    for (x, y) in [
        (w * 0.25, h * 0.25),
        (w * 0.75, h * 0.25),
        (w * 0.5, h * 0.75),
    ] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.5);
        push_f32(&mut vb_payload, 1.0);
        push_u32(&mut vb_payload, green_argb);
    }
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // SET_RENDER_TARGETS: RT0 = texture 1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, tex_width as f32);
    push_f32(&mut stream, tex_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, VTX_STRIDE_D3D9);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY TRIANGLELIST.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 3) guest-backed destination.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_TEXTURE);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, tex_width);
    push_u32(&mut stream, tex_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, tex_pitch);
    push_u32(&mut stream, 1); // backing_alloc_id
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // COPY_TEXTURE2D 1 -> 3 with WRITEBACK_DST.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CopyTexture2d as u32);
    push_u32(&mut stream, 64);
    push_u32(&mut stream, 3); // dst_texture
    push_u32(&mut stream, 1); // src_texture
    push_u32(&mut stream, 0); // dst_mip_level
    push_u32(&mut stream, 0); // dst_array_layer
    push_u32(&mut stream, 0); // src_mip_level
    push_u32(&mut stream, 0); // src_array_layer
    push_u32(&mut stream, 0); // dst_x
    push_u32(&mut stream, 0); // dst_y
    push_u32(&mut stream, 0); // src_x
    push_u32(&mut stream, 0); // src_y
    push_u32(&mut stream, tex_width);
    push_u32(&mut stream, tex_height);
    push_u32(&mut stream, cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let center = read_pixel_bgra(
        &mut mem,
        dst_alloc_gpa,
        tex_pitch,
        tex_width / 2,
        tex_height / 2,
    );
    let corner = read_pixel_bgra(&mut mem, dst_alloc_gpa, tex_pitch, 5, 5);
    assert_eq!(center & 0x00FF_FFFF, 0x0000_FF00);
    assert_eq!(corner & 0x00FF_FFFF, 0x00FF_0000);
}

#[test]
fn cmd_exec_d3d11_scissor_clips_draw_when_enabled() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float2) + COLOR(float4).
    const VTX_STRIDE_D3D11: u32 = 24;
    let mut vb_payload = Vec::with_capacity(3 * (VTX_STRIDE_D3D11 as usize));
    // Fullscreen triangle.
    for (x, y) in [(-1.0f32, -1.0f32), (-1.0f32, 3.0f32), (3.0f32, -1.0f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        // COLOR RGBA = green
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2); // element_count
    push_u32(&mut ilay, 0);
    // POSITION
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16); // DXGI_FORMAT_R32G32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 0); // offset 0
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    // COLOR
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 8); // offset 8
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1); // AEROGPU_FORMAT_B8G8R8A8_UNORM
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1); // backing_alloc_id
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 2) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 3) with ILAY blob.
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS: RT0 = texture 1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, VTX_STRIDE_D3D11);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY TRIANGLELIST.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // SET_RASTERIZER_STATE: enable scissor, no cull.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetRasterizerState as u32,
    );
    push_u32(&mut stream, 32);
    push_u32(&mut stream, cmd::AerogpuFillMode::Solid as u32);
    push_u32(&mut stream, cmd::AerogpuCullMode::None as u32);
    push_u32(&mut stream, 0); // front_ccw
    push_u32(&mut stream, 1); // scissor_enable
    push_i32(&mut stream, 0); // depth_bias
    push_u32(&mut stream, 0);

    // SET_SCISSOR to left half.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetScissor as u32);
    push_u32(&mut stream, 24);
    push_i32(&mut stream, 0);
    push_i32(&mut stream, 0);
    push_i32(&mut stream, (rt_width / 2) as i32);
    push_i32(&mut stream, rt_height as i32);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let inside = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 5, rt_height / 2);
    let outside = read_pixel_bgra(
        &mut mem,
        rt_alloc_gpa,
        rt_pitch,
        rt_width - 5,
        rt_height / 2,
    );
    assert_eq!(inside & 0x00FF_FFFF, 0x0000_FF00);
    assert_eq!(outside & 0x00FF_FFFF, 0x00FF_0000);
}

#[test]
fn cmd_exec_d3d11_cull_mode_culls_ccw_when_front_ccw_false() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float2) + COLOR(float4).
    const VTX_STRIDE_D3D11: u32 = 24;
    let mut vb_payload = Vec::with_capacity(3 * (VTX_STRIDE_D3D11 as usize));
    // CCW triangle in clip space.
    for (x, y) in [(-0.5f32, -0.5f32), (0.5f32, -0.5f32), (0.0f32, 0.5f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 8);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 2) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 3).
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, VTX_STRIDE_D3D11);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // SET_RASTERIZER_STATE: cull back, front_ccw=false (CCW triangle should be culled).
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetRasterizerState as u32,
    );
    push_u32(&mut stream, 32);
    push_u32(&mut stream, cmd::AerogpuFillMode::Solid as u32);
    push_u32(&mut stream, cmd::AerogpuCullMode::Back as u32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_i32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let center = read_pixel_bgra(
        &mut mem,
        rt_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    );
    assert_eq!(center & 0x00FF_FFFF, 0x00FF_0000);
}

#[test]
fn cmd_exec_d3d11_cull_mode_keeps_ccw_when_front_ccw_true() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float2) + COLOR(float4).
    const VTX_STRIDE_D3D11: u32 = 24;
    let mut vb_payload = Vec::with_capacity(3 * (VTX_STRIDE_D3D11 as usize));
    for (x, y) in [(-0.5f32, -0.5f32), (0.5f32, -0.5f32), (0.0f32, 0.5f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 8);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 2) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 3).
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, VTX_STRIDE_D3D11);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // SET_RASTERIZER_STATE: cull back, front_ccw=true (CCW triangle should be visible).
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetRasterizerState as u32,
    );
    push_u32(&mut stream, 32);
    push_u32(&mut stream, cmd::AerogpuFillMode::Solid as u32);
    push_u32(&mut stream, cmd::AerogpuCullMode::Back as u32);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_i32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let center = read_pixel_bgra(
        &mut mem,
        rt_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    );
    assert_eq!(center & 0x00FF_FFFF, 0x0000_FF00);
}

#[test]
fn cmd_exec_d3d11_depth_clip_toggle_clips_triangle_when_enabled() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Two guest-backed render targets (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_clip_alloc_gpa = 0x6000u64;
    let rt_no_clip_alloc_gpa = rt_clip_alloc_gpa + rt_bytes;

    // Allocation table (two entries).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + 2 * ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 2);
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    // alloc_id 1 -> clipped RT
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_clip_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    // alloc_id 2 -> unclipped RT
    push_u32(&mut alloc_table, 2);
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_no_clip_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float3) + COLOR(float4). Z is outside the D3D clip volume.
    const VTX_STRIDE: u32 = 28;
    let mut vb_payload = Vec::with_capacity(3 * (VTX_STRIDE as usize));
    for (x, y) in [(-1.0f32, -1.0f32), (-1.0f32, 3.0f32), (3.0f32, -1.0f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, -0.5); // z (outside [0, 1])
                                         // COLOR RGBA = red
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    // POSITION (float3)
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 6); // DXGI_FORMAT_R32G32B32_FLOAT
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    // COLOR (float4)
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 12);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, AeroGpuFormat::B8G8R8A8Unorm as u32);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 2) backed by alloc_id=2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, AeroGpuFormat::B8G8R8A8Unorm as u32);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 3) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 4).
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 4.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // SET_VIEWPORT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, VTX_STRIDE);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(
        &mut stream,
        cmd::AerogpuPrimitiveTopology::TriangleList as u32,
    );
    push_u32(&mut stream, 0);

    for (rt_handle, depth_clip_enable) in [(1u32, true), (2u32, false)] {
        // SET_RENDER_TARGETS.
        push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
        push_u32(&mut stream, 48);
        push_u32(&mut stream, 1);
        push_u32(&mut stream, 0);
        push_u32(&mut stream, rt_handle);
        for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
            push_u32(&mut stream, 0);
        }

        // SET_RASTERIZER_STATE: DepthClipEnable is represented by the absence/presence of the
        // DEPTH_CLIP_DISABLE flag bit.
        push_u32(
            &mut stream,
            cmd::AerogpuCmdOpcode::SetRasterizerState as u32,
        );
        push_u32(&mut stream, 32);
        push_u32(&mut stream, cmd::AerogpuFillMode::Solid as u32);
        push_u32(&mut stream, cmd::AerogpuCullMode::None as u32);
        push_u32(&mut stream, 0); // front_ccw
        push_u32(&mut stream, 0); // scissor_enable
        push_i32(&mut stream, 0); // depth_bias
        let flags = if depth_clip_enable {
            0
        } else {
            cmd::AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE
        };
        push_u32(&mut stream, flags);

        // CLEAR black.
        push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
        push_u32(&mut stream, 36);
        push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
        push_f32(&mut stream, 0.0);
        push_f32(&mut stream, 0.0);
        push_f32(&mut stream, 0.0);
        push_f32(&mut stream, 1.0);
        push_f32(&mut stream, 1.0);
        push_u32(&mut stream, 0);

        // DRAW 3 verts.
        push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
        push_u32(&mut stream, 24);
        push_u32(&mut stream, 3);
        push_u32(&mut stream, 1);
        push_u32(&mut stream, 0);
        push_u32(&mut stream, 0);
    }

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let clipped = read_pixel_bgra(
        &mut mem,
        rt_clip_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    );
    let unclipped = read_pixel_bgra(
        &mut mem,
        rt_no_clip_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    );
    assert_eq!(clipped & 0x00FF_FFFF, 0);
    assert_eq!(unclipped & 0x00FF_FFFF, 0x00FF_0000);
}

fn exec_d3d11_fullscreen_triangle_center_pixel(push_blend_state: impl FnOnce(&mut Vec<u8>)) -> u32 {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float2) + COLOR(float4).
    const VTX_STRIDE_D3D11: u32 = 24;
    let mut vb_payload = Vec::with_capacity(3 * (VTX_STRIDE_D3D11 as usize));
    // Fullscreen triangle with alpha=0.5.
    for (x, y) in [(-1.0f32, -1.0f32), (-1.0f32, 3.0f32), (3.0f32, -1.0f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        // COLOR RGBA = green, a=0.5
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 0.5);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 8);
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 2) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 3).
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, VTX_STRIDE_D3D11);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // SET_BLEND_STATE.
    push_blend_state(&mut stream);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    read_pixel_bgra(
        &mut mem,
        rt_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    )
}

#[test]
fn cmd_exec_d3d11_alpha_blend_matches_src_alpha_over() {
    let center = exec_d3d11_fullscreen_triangle_center_pixel(|stream| {
        // Standard src_alpha/inv_src_alpha.
        //
        // The protocol's blend state carries a full D3D11-style payload (separate alpha factors,
        // blend constant, sample mask). For this test we only care about RGB blending; keep the
        // remaining fields at sensible defaults.
        push_u32(stream, cmd::AerogpuCmdOpcode::SetBlendState as u32);
        push_u32(stream, cmd::AerogpuCmdSetBlendState::SIZE_BYTES as u32);
        // BlendState { enable, src_factor, dst_factor, blend_op, ... }
        push_u32(stream, 1); // enable
        push_u32(stream, cmd::AerogpuBlendFactor::SrcAlpha as u32);
        push_u32(stream, cmd::AerogpuBlendFactor::InvSrcAlpha as u32);
        push_u32(stream, cmd::AerogpuBlendOp::Add as u32);
        // color_write_mask (u8) + reserved0[3]
        push_u32(stream, 0xF);
        // Alpha blend factors/op.
        push_u32(stream, cmd::AerogpuBlendFactor::SrcAlpha as u32);
        push_u32(stream, cmd::AerogpuBlendFactor::InvSrcAlpha as u32);
        push_u32(stream, cmd::AerogpuBlendOp::Add as u32);
        // blend_constant_rgba_f32
        push_f32(stream, 0.0);
        push_f32(stream, 0.0);
        push_f32(stream, 0.0);
        push_f32(stream, 0.0);
        // sample_mask
        push_u32(stream, u32::MAX);
    });
    let b = (center & 0xFF) as u8;
    let g = ((center >> 8) & 0xFF) as u8;
    let r = ((center >> 16) & 0xFF) as u8;

    assert_eq!(b, 0, "unexpected blue channel; center={center:#010x}");
    assert!(
        (r as i32 - 0x80).abs() <= 2,
        "unexpected red channel; center={center:#010x} r={r:#04x} g={g:#04x} b={b:#04x}",
    );
    assert!(
        (g as i32 - 0x80).abs() <= 2,
        "unexpected green channel; center={center:#010x} r={r:#04x} g={g:#04x} b={b:#04x}",
    );
}

#[test]
fn cmd_exec_d3d11_blend_factor_constant_matches_expected() {
    let center = exec_d3d11_fullscreen_triangle_center_pixel(|stream| {
        push_u32(stream, cmd::AerogpuCmdOpcode::SetBlendState as u32);
        push_u32(stream, cmd::AerogpuCmdSetBlendState::SIZE_BYTES as u32);
        push_u32(stream, 1); // enable
        push_u32(stream, cmd::AerogpuBlendFactor::Constant as u32);
        push_u32(stream, cmd::AerogpuBlendFactor::InvConstant as u32);
        push_u32(stream, cmd::AerogpuBlendOp::Add as u32);
        push_u32(stream, 0xF); // write mask + padding
                               // Alpha blend: out_a = src_a
        push_u32(stream, cmd::AerogpuBlendFactor::One as u32);
        push_u32(stream, cmd::AerogpuBlendFactor::Zero as u32);
        push_u32(stream, cmd::AerogpuBlendOp::Add as u32);
        for _ in 0..4 {
            push_f32(stream, 0.25);
        }
        push_u32(stream, u32::MAX);
    });

    let b = (center & 0xFF) as i32;
    let g = ((center >> 8) & 0xFF) as i32;
    let r = ((center >> 16) & 0xFF) as i32;
    let a = ((center >> 24) & 0xFF) as i32;

    assert!((r - 0xBF).abs() <= 2, "r={r}");
    assert!((g - 0x40).abs() <= 2, "g={g}");
    assert!(b.abs() <= 2, "b={b}");
    assert!((a - 0x80).abs() <= 2, "a={a}");
}

#[test]
fn cmd_exec_d3d11_sample_mask_discards_draw() {
    let center = exec_d3d11_fullscreen_triangle_center_pixel(|stream| {
        push_u32(stream, cmd::AerogpuCmdOpcode::SetBlendState as u32);
        push_u32(stream, cmd::AerogpuCmdSetBlendState::SIZE_BYTES as u32);
        push_u32(stream, 1); // enable
        push_u32(stream, cmd::AerogpuBlendFactor::Constant as u32);
        push_u32(stream, cmd::AerogpuBlendFactor::InvConstant as u32);
        push_u32(stream, cmd::AerogpuBlendOp::Add as u32);
        push_u32(stream, 0xF);
        push_u32(stream, cmd::AerogpuBlendFactor::One as u32);
        push_u32(stream, cmd::AerogpuBlendFactor::Zero as u32);
        push_u32(stream, cmd::AerogpuBlendOp::Add as u32);
        for _ in 0..4 {
            push_f32(stream, 0.25);
        }
        // sample_mask bit 0 is cleared => draw is discarded for sample_count=1.
        push_u32(stream, 0);
    });

    let b = (center & 0xFF) as u8;
    let g = ((center >> 8) & 0xFF) as u8;
    let r = ((center >> 16) & 0xFF) as u8;
    let a = ((center >> 24) & 0xFF) as u8;

    assert_eq!(b, 0);
    assert_eq!(g, 0);
    assert_eq!(r, 0xFF);
    assert_eq!(a, 0xFF);
}

#[test]
fn cmd_exec_d3d11_legacy_blend_state_packet_still_works() {
    let center = exec_d3d11_fullscreen_triangle_center_pixel(|stream| {
        // Legacy packet size (28 bytes): opcode+size+5 u32s.
        push_u32(stream, cmd::AerogpuCmdOpcode::SetBlendState as u32);
        push_u32(stream, 28);
        push_u32(stream, 1); // enable
        push_u32(stream, cmd::AerogpuBlendFactor::SrcAlpha as u32);
        push_u32(stream, cmd::AerogpuBlendFactor::InvSrcAlpha as u32);
        push_u32(stream, cmd::AerogpuBlendOp::Add as u32);
        push_u32(stream, 0xF); // write mask + padding
    });
    let b = (center & 0xFF) as u8;
    let g = ((center >> 8) & 0xFF) as u8;
    let r = ((center >> 16) & 0xFF) as u8;

    assert_eq!(b, 0, "expected b=0, got {b:#x} (pixel={center:#010x})");
    assert!(
        (r as i32 - 0x80).abs() <= 2,
        "expected r≈0x80, got {r:#x} (pixel={center:#010x})"
    );
    assert!(
        (g as i32 - 0x80).abs() <= 2,
        "expected g≈0x80, got {g:#x} (pixel={center:#010x})"
    );
}

#[test]
fn cmd_exec_d3d11_depth_test_rejects_farther_triangle() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float3) + COLOR(float4).
    const VTX_STRIDE_D3D11: u32 = 28;
    let mut vb_payload = Vec::with_capacity(6 * (VTX_STRIDE_D3D11 as usize));
    // Near triangle (blue) at z=0.2.
    for (x, y) in [(-0.5f32, -0.5f32), (0.0f32, 0.5f32), (0.5f32, -0.5f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.2);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 1.0);
    }
    // Far triangle (green) at z=0.8.
    for (x, y) in [(-0.5f32, -0.5f32), (0.0f32, 0.5f32), (0.5f32, -0.5f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.8);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2); // element_count
    push_u32(&mut ilay, 0);
    // POSITION
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 6); // DXGI_FORMAT_R32G32B32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 0); // offset 0
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    // COLOR
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 12); // offset 12
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, AeroGpuFormat::B8G8R8A8Unorm as u32);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1); // backing_alloc_id
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 2) depth buffer (host allocated).
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL);
    push_u32(&mut stream, AeroGpuFormat::D32Float as u32);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0); // row_pitch_bytes (auto)
    push_u32(&mut stream, 0); // backing_alloc_id
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 3) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 4) with ILAY blob.
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 4.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS: RT0 = texture 1, depth = texture 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, VTX_STRIDE_D3D11);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(
        &mut stream,
        cmd::AerogpuPrimitiveTopology::TriangleList as u32,
    );
    push_u32(&mut stream, 0);

    // SET_DEPTH_STENCIL_STATE: depth test enabled, write enabled, func=LESS.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetDepthStencilState as u32,
    );
    push_u32(&mut stream, 28);
    push_u32(&mut stream, 1); // depth_enable
    push_u32(&mut stream, 1); // depth_write_enable
    push_u32(&mut stream, cmd::AerogpuCompareFunc::Less as u32);
    push_u32(&mut stream, 0); // stencil_enable
    push_u32(&mut stream, 0x0000_FFFF); // read/write masks + reserved

    // CLEAR red + depth=1.0.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(
        &mut stream,
        cmd::AEROGPU_CLEAR_COLOR | cmd::AEROGPU_CLEAR_DEPTH,
    );
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0); // depth
    push_u32(&mut stream, 0); // stencil

    // DRAW 6 verts (2 triangles).
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 6);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let corner = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 0, 0);
    let center = read_pixel_bgra(
        &mut mem,
        rt_alloc_gpa,
        rt_pitch,
        rt_width / 2,
        rt_height / 2,
    );

    assert_eq!(corner & 0x00FF_FFFF, 0x00FF_0000);
    assert_eq!(center & 0x00FF_FFFF, 0x0000_00FF);
}

#[test]
fn cmd_exec_d3d11_texture_sampling_point_clamp_matches_expected_texels() {
    fn pack_bgra(b: u8, g: u8, r: u8, a: u8) -> u32 {
        (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | (b as u32)
    }

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = new_test_device(AeroGpuDeviceConfig::default());

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size =
        ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1);
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Source texture upload: 4x4 BGRA.
    const SRC_W: u32 = 4;
    const SRC_H: u32 = 4;
    let src_pixels: [u32; 16] = [
        // Row 0
        pack_bgra(0, 0, 255, 255),     // red
        pack_bgra(0, 255, 0, 255),     // green
        pack_bgra(255, 0, 0, 255),     // blue
        pack_bgra(255, 255, 255, 255), // white
        // Row 1
        pack_bgra(0, 255, 255, 255), // yellow
        pack_bgra(255, 255, 0, 255), // cyan
        pack_bgra(255, 0, 255, 255), // magenta
        pack_bgra(0, 0, 0, 255),     // black
        // Row 2
        pack_bgra(255, 0, 0, 255),   // blue
        pack_bgra(0, 0, 255, 255),   // red
        pack_bgra(255, 0, 255, 255), // magenta
        pack_bgra(0, 255, 0, 255),   // green
        // Row 3
        pack_bgra(255, 255, 0, 255),   // cyan
        pack_bgra(0, 255, 255, 255),   // yellow
        pack_bgra(255, 255, 255, 255), // white
        pack_bgra(255, 0, 0, 255),     // blue
    ];
    let mut src_payload = Vec::with_capacity((SRC_W * SRC_H * 4) as usize);
    for p in src_pixels {
        push_u32(&mut src_payload, p);
    }

    // Vertex buffer payload: POSITION(float2) + TEXCOORD(float2).
    const VTX_STRIDE: u32 = 16;
    let mut vb_payload = Vec::with_capacity(4 * (VTX_STRIDE as usize));
    // Top-left
    push_f32(&mut vb_payload, -1.0);
    push_f32(&mut vb_payload, 1.0);
    push_f32(&mut vb_payload, 0.0);
    push_f32(&mut vb_payload, 0.0);
    // Top-right
    push_f32(&mut vb_payload, 1.0);
    push_f32(&mut vb_payload, 1.0);
    push_f32(&mut vb_payload, 1.0);
    push_f32(&mut vb_payload, 0.0);
    // Bottom-right
    push_f32(&mut vb_payload, 1.0);
    push_f32(&mut vb_payload, -1.0);
    push_f32(&mut vb_payload, 1.0);
    push_f32(&mut vb_payload, 1.0);
    // Bottom-left
    push_f32(&mut vb_payload, -1.0);
    push_f32(&mut vb_payload, -1.0);
    push_f32(&mut vb_payload, 0.0);
    push_f32(&mut vb_payload, 1.0);

    // Index buffer payload: 0,1,2,0,2,3 (u16).
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
    let mut ib_payload = Vec::with_capacity(indices.len() * 2);
    for idx in indices {
        ib_payload.extend_from_slice(&idx.to_le_bytes());
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2); // element_count
    push_u32(&mut ilay, 0);
    // POSITION
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16); // DXGI_FORMAT_R32G32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 0); // offset 0
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    // TEXCOORD0
    push_u32(&mut ilay, fnv1a32("TEXCOORD"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16); // DXGI_FORMAT_R32G32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 8); // offset 8
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, AeroGpuFormat::B8G8R8A8Unorm as u32);
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1); // backing_alloc_id
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 2) host-allocated source texture.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_TEXTURE);
    push_u32(&mut stream, AeroGpuFormat::B8G8R8A8Unorm as u32);
    push_u32(&mut stream, SRC_W);
    push_u32(&mut stream, SRC_H);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0); // row_pitch_bytes (auto)
    push_u32(&mut stream, 0); // backing_alloc_id
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into src texture.
    let upload_src_size_no_pad = 32 + src_payload.len();
    let upload_src_size = (upload_src_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_src_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, src_payload.len() as u64);
    stream.extend_from_slice(&src_payload);
    stream.resize(stream.len() + (upload_src_size - upload_src_size_no_pad), 0);

    // CREATE_BUFFER (handle 3) host-allocated vertex buffer.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_vb_size_no_pad = 32 + vb_payload.len();
    let upload_vb_size = (upload_vb_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_vb_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_vb_size - upload_vb_size_no_pad), 0);

    // CREATE_BUFFER (handle 4) host-allocated index buffer.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_INDEX_BUFFER);
    push_u64(&mut stream, ib_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into IB.
    let upload_ib_size_no_pad = 32 + ib_payload.len();
    let upload_ib_size = (upload_ib_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_ib_size as u32);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, ib_payload.len() as u64);
    stream.extend_from_slice(&ib_payload);
    stream.resize(stream.len() + (upload_ib_size - upload_ib_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 5) with ILAY blob.
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 5);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // CREATE_SAMPLER (handle 6): point sampling + clamp.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateSampler as u32);
    push_u32(&mut stream, 28);
    push_u32(&mut stream, 6);
    push_u32(&mut stream, cmd::AerogpuSamplerFilter::Nearest as u32);
    push_u32(
        &mut stream,
        cmd::AerogpuSamplerAddressMode::ClampToEdge as u32,
    );
    push_u32(
        &mut stream,
        cmd::AerogpuSamplerAddressMode::ClampToEdge as u32,
    );
    push_u32(
        &mut stream,
        cmd::AerogpuSamplerAddressMode::ClampToEdge as u32,
    );

    // SET_INPUT_LAYOUT = 5.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 5);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS: RT0 = texture 1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, VTX_STRIDE);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_INDEX_BUFFER -> buffer 4, uint16.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetIndexBuffer as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, cmd::AerogpuIndexFormat::Uint16 as u32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY.
    push_u32(
        &mut stream,
        cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32,
    );
    push_u32(&mut stream, 16);
    push_u32(
        &mut stream,
        cmd::AerogpuPrimitiveTopology::TriangleList as u32,
    );
    push_u32(&mut stream, 0);

    // SET_TEXTURE: PS t0 = texture 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetTexture as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, cmd::AerogpuShaderStage::Pixel as u32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);

    // SET_SAMPLERS: PS s0 = sampler 6 (point+clamp).
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetSamplers as u32);
    push_u32(&mut stream, 28);
    push_u32(&mut stream, cmd::AerogpuShaderStage::Pixel as u32);
    push_u32(&mut stream, 0); // start_slot
    push_u32(&mut stream, 1); // sampler_count
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 6);

    // Mirror D3D11 by also binding to VS, even though the software executor samples in PS.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetSamplers as u32);
    push_u32(&mut stream, 28);
    push_u32(&mut stream, cmd::AerogpuShaderStage::Vertex as u32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 6);

    // CLEAR black.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW_INDEXED 6 indices.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::DrawIndexed as u32);
    push_u32(&mut stream, 28);
    push_u32(&mut stream, 6);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_i32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);

    assert_eq!(dev.regs.completed_fence, 1);

    let p0 = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 8, 8);
    let p1 = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 56, 8);
    let p2 = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 8, 56);
    let p3 = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 40, 40);

    assert_eq!(p0 & 0x00FF_FFFF, 0x00FF_0000);
    assert_eq!(p1 & 0x00FF_FFFF, 0x00FF_FFFF);
    assert_eq!(p2 & 0x00FF_FFFF, 0x0000_FFFF);
    assert_eq!(p3 & 0x00FF_FFFF, 0x00FF_00FF);
}
