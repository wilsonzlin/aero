use aero_devices_gpu::pci::AeroGpuDeviceConfig;
use aero_devices_gpu::regs::{irq_bits, mmio, ring_control, AEROGPU_MMIO_MAGIC};
use aero_devices_gpu::ring::{
    AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC,
    FENCE_PAGE_MAGIC_OFFSET, RING_HEAD_OFFSET, RING_TAIL_OFFSET,
};
use aero_devices_gpu::AeroGpuPciDevice;
use aero_devices::pci::PciDevice;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuRingHeader as ProtocolRingHeader;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuSubmitDesc as ProtocolSubmitDesc;
use memory::{MemoryBus, MmioHandler};

const RING_MAGIC_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, magic) as u64;
const RING_ABI_VERSION_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, abi_version) as u64;
const RING_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, size_bytes) as u64;
const RING_ENTRY_COUNT_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, entry_count) as u64;
const RING_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolRingHeader, entry_stride_bytes) as u64;
const RING_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, flags) as u64;

const SUBMIT_DESC_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, desc_size_bytes) as u64;
const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, signal_fence) as u64;

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
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES);
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

    // Once bus mastering is enabled, the same doorbell should process.
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev.write(mmio::DOORBELL, 4, 1);
    dev.tick(&mut mem, 0);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
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
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES);
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
