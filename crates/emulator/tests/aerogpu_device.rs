use std::time::{Duration, Instant};

use aero_protocol::aerogpu::aerogpu_cmd::{AerogpuCmdOpcode, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_PRESENT_FLAG_VSYNC};
use emulator::devices::aerogpu_regs::{irq_bits, mmio, ring_control, AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, AEROGPU_MMIO_MAGIC};
use emulator::devices::aerogpu_ring::{AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC};
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::io::pci::MmioDevice;
use memory::MemoryBus;

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
fn doorbell_updates_ring_head_fence_page_and_irq() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    assert_eq!(dev.mmio_read(&mut mem, mmio::MAGIC, 4), AEROGPU_MMIO_MAGIC);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header.
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, 0); // cmd_gpa
    mem.write_u32(desc_gpa + 24, 0); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 42); // signal_fence

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

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + 24);
    assert_eq!(head_after, 1);

    assert_eq!(mem.read_u32(fence_gpa + 0), AEROGPU_FENCE_PAGE_MAGIC);
    assert_eq!(mem.read_u64(fence_gpa + 8), 42);

    dev.mmio_write(&mut mem, mmio::IRQ_ACK, 4, irq_bits::FENCE);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());
}

#[test]
fn doorbell_accepts_newer_minor_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header: advertise a newer minor version while keeping the same major.
    let newer_minor = (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1);
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, newer_minor);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, 0); // cmd_gpa
    mem.write_u32(desc_gpa + 24, 0); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 42); // signal_fence

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

    assert_eq!(dev.regs.completed_fence, 42);
    assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + 24);
    assert_eq!(head_after, 1);

    assert_eq!(mem.read_u32(fence_gpa + 0), AEROGPU_FENCE_PAGE_MAGIC);
    assert_eq!(mem.read_u64(fence_gpa + 8), 42);
}

#[test]
fn doorbell_accepts_larger_submit_desc_stride_and_size() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 128u32;

    // Ring header.
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Submit descriptor at slot 0 with an extended size.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 128); // desc_size_bytes
    mem.write_u64(desc_gpa + 48, 42); // signal_fence

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_eq!(dev.regs.stats.malformed_submissions, 0);
    assert_eq!(mem.read_u32(ring_gpa + 24), 1);
    assert_eq!(dev.regs.completed_fence, 42);
}

#[test]
fn doorbell_rejects_unknown_major_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header: advertise an unsupported major version.
    let unsupported_major = ((AEROGPU_ABI_MAJOR + 1) << 16) | 0;
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, unsupported_major);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Submit descriptor at slot 0 (should not be processed due to ABI mismatch).
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
    mem.write_u64(desc_gpa + 48, 42); // signal_fence

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

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());

    // Ring and fence state should remain unchanged.
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + 24), 0);
    assert_eq!(mem.read_u32(fence_gpa + 0), 0);
}

#[test]
fn scanout_bgra_converts_to_rgba() {
    let mut mem = VecMemory::new(0x20_000);
    let dev = &mut AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    let fb_gpa = 0x5000u64;
    // 2x1 pixels, BGRA: (R=1,G=2,B=3,A=4), (R=10,G=20,B=30,A=40).
    mem.write_physical(fb_gpa, &[3, 2, 1, 4, 30, 20, 10, 40]);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 2);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FORMAT, 4, AeroGpuFormat::B8G8R8A8Unorm as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 8);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![1, 2, 3, 4, 10, 20, 30, 40]);
}

#[test]
fn vblank_tick_sets_irq_status() {
    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vblank_hz = Some(10);
    let mut mem = VecMemory::new(0x1000);
    let mut dev = AeroGpuPciDevice::new(cfg, 0);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK);

    let t0 = Instant::now();
    dev.tick(&mut mem, t0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    dev.tick(&mut mem, t0 + Duration::from_millis(100));
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

#[test]
fn vsynced_present_fence_completes_on_vblank() {
    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vblank_hz = Some(10);

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = AeroGpuPciDevice::new(cfg, 0);

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = 40u32;

    mem.write_u32(cmd_gpa + 0, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(cmd_gpa + 4, dev.regs.abi_version);
    mem.write_u32(cmd_gpa + 8, cmd_size_bytes);
    mem.write_u32(cmd_gpa + 12, 0); // flags
    mem.write_u32(cmd_gpa + 16, 0);
    mem.write_u32(cmd_gpa + 20, 0);

    // aerogpu_cmd_present
    mem.write_u32(cmd_gpa + 24, AerogpuCmdOpcode::Present as u32);
    mem.write_u32(cmd_gpa + 28, 16); // size_bytes
    mem.write_u32(cmd_gpa + 32, 0); // scanout_id
    mem.write_u32(cmd_gpa + 36, AEROGPU_PRESENT_FLAG_VSYNC);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, AeroGpuSubmitDesc::FLAG_PRESENT); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + 24, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 42); // signal_fence

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

    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + 24);
    assert_eq!(head_after, 1);

    let t0 = Instant::now();
    dev.tick(&mut mem, t0);
    assert_eq!(dev.regs.completed_fence, 0);

    dev.tick(&mut mem, t0 + Duration::from_millis(100));

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(mem.read_u32(fence_gpa + 0), AEROGPU_FENCE_PAGE_MAGIC);
    assert_eq!(mem.read_u64(fence_gpa + 8), 42);
}
