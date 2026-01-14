#![cfg(feature = "aerogpu-legacy")]

use aero_protocol::aerogpu::aerogpu_pci as proto;
use emulator::devices::pci::aerogpu_legacy::{AeroGpuLegacyDeviceConfig, AeroGpuLegacyPciDevice};
use memory::MemoryBus;
use std::time::{Duration, Instant};

fn read_u8(dev: &AeroGpuLegacyPciDevice, offset: u16) -> u8 {
    dev.config_read(offset, 1) as u8
}

fn new_test_device(cfg: AeroGpuLegacyDeviceConfig) -> AeroGpuLegacyPciDevice {
    let mut dev = AeroGpuLegacyPciDevice::new(cfg, 0);
    // Enable PCI MMIO decode + bus mastering so MMIO/DMA paths behave like real PCI hardware.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev
}

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

// Legacy MMIO offsets from `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`.
mod mmio {
    pub const RING_BASE_LO: u64 = 0x0010;
    pub const RING_BASE_HI: u64 = 0x0014;
    pub const RING_ENTRY_COUNT: u64 = 0x0018;
    pub const RING_HEAD: u64 = 0x001c;
    pub const RING_TAIL: u64 = 0x0020;
    pub const RING_DOORBELL: u64 = 0x0024;
    pub const INT_STATUS: u64 = 0x0030;
    pub const INT_ACK: u64 = 0x0034;
    pub const FENCE_COMPLETED: u64 = 0x0038;

    pub const SCANOUT_FB_LO: u64 = 0x0100;
    pub const SCANOUT_FB_HI: u64 = 0x0104;
    pub const SCANOUT_PITCH: u64 = 0x0108;
    pub const SCANOUT_WIDTH: u64 = 0x010c;
    pub const SCANOUT_HEIGHT: u64 = 0x0110;
    pub const SCANOUT_FORMAT: u64 = 0x0114;
    pub const SCANOUT_ENABLE: u64 = 0x0118;

    pub const IRQ_STATUS: u64 = 0x0300;
    pub const IRQ_ENABLE: u64 = 0x0304;
    pub const IRQ_ACK: u64 = 0x0308;

    pub const SCANOUT0_VBLANK_SEQ_LO: u64 = 0x0420;
    pub const SCANOUT0_VBLANK_SEQ_HI: u64 = 0x0424;
    pub const SCANOUT0_VBLANK_TIME_NS_LO: u64 = 0x0428;
    pub const SCANOUT0_VBLANK_TIME_NS_HI: u64 = 0x042c;
}

#[test]
fn pci_identity_class_codes_match_aero_protocol() {
    let dev = AeroGpuLegacyPciDevice::new(AeroGpuLegacyDeviceConfig::default(), 0);
    assert_eq!(read_u8(&dev, 0x09), proto::AEROGPU_PCI_PROG_IF);
    assert_eq!(
        read_u8(&dev, 0x0a),
        proto::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE
    );
    assert_eq!(
        read_u8(&dev, 0x0b),
        proto::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER
    );
}

#[test]
fn doorbell_updates_ring_head_and_fence_irq() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuLegacyDeviceConfig::default());

    // Ring in guest memory.
    let ring_gpa = 0x1000u64;
    let desc_gpa = 0x2000u64;
    let entry_count = 8u32;

    // Ring entry at index 0 (24 bytes):
    // submit.type=1, flags=0, fence=42, desc_size=32, desc_gpa=0x2000
    mem.write_u32(ring_gpa, 1);
    mem.write_u32(ring_gpa + 4, 0);
    mem.write_u32(ring_gpa + 8, 42);
    mem.write_u32(ring_gpa + 12, 32);
    mem.write_u64(ring_gpa + 16, desc_gpa);

    // Submission descriptor header at desc_gpa (32 bytes). Only version/fence are validated.
    mem.write_u32(desc_gpa, 1); // version
    mem.write_u32(desc_gpa + 4, 1); // type (render)
    mem.write_u32(desc_gpa + 8, 42); // fence
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, 0);
    mem.write_u32(desc_gpa + 24, 0);
    mem.write_u32(desc_gpa + 28, 0);

    dev.mmio_write(&mut mem, mmio::RING_BASE_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_BASE_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_ENTRY_COUNT, 4, entry_count);

    // head=0, tail=1, ring doorbell.
    dev.mmio_write(&mut mem, mmio::RING_HEAD, 4, 0);
    dev.mmio_write(&mut mem, mmio::RING_TAIL, 4, 1);
    dev.mmio_write(&mut mem, mmio::RING_DOORBELL, 4, 1);

    assert_eq!(dev.mmio_read(&mut mem, mmio::RING_HEAD, 4), 1);
    assert_eq!(dev.mmio_read(&mut mem, mmio::FENCE_COMPLETED, 4), 42);
    assert_ne!(dev.mmio_read(&mut mem, mmio::INT_STATUS, 4) & 1, 0);
    assert!(dev.irq_level());

    // Ack clears the interrupt and deasserts the line.
    dev.mmio_write(&mut mem, mmio::INT_ACK, 4, 1);
    assert_eq!(dev.mmio_read(&mut mem, mmio::INT_STATUS, 4) & 1, 0);
    assert!(!dev.irq_level());
}

#[test]
fn scanout_x8r8g8b8_converts_to_rgba() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuLegacyDeviceConfig::default());

    let fb_gpa = 0x3000u64;
    // 2x1 pixels, X8R8G8B8 (little-endian bytes = B,G,R,X).
    mem.write_physical(fb_gpa, &[3, 2, 1, 0, 30, 20, 10, 0]);

    dev.mmio_write(&mut mem, mmio::SCANOUT_FB_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT_FB_HI, 4, (fb_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT_PITCH, 4, 8);
    dev.mmio_write(&mut mem, mmio::SCANOUT_WIDTH, 4, 2);
    dev.mmio_write(&mut mem, mmio::SCANOUT_HEIGHT, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT_FORMAT, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT_ENABLE, 4, 1);

    let rgba = dev.read_scanout_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![1, 2, 3, 0xff, 10, 20, 30, 0xff]);
}

#[test]
fn scanout_fb_gpa_updates_are_atomic_for_readback() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = new_test_device(AeroGpuLegacyDeviceConfig::default());

    let fb0 = 0x3000u64;
    let fb1 = 0x4000u64;

    // 1x1 pixels, X8R8G8B8 (little-endian bytes = B,G,R,X).
    mem.write_physical(fb0, &[1, 2, 3, 0]);
    mem.write_physical(fb1, &[10, 20, 30, 0]);

    dev.mmio_write(&mut mem, mmio::SCANOUT_FB_LO, 4, fb0 as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT_FB_HI, 4, (fb0 >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT_PITCH, 4, 4);
    dev.mmio_write(&mut mem, mmio::SCANOUT_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT_HEIGHT, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT_FORMAT, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT_ENABLE, 4, 1);

    let rgba0 = dev.read_scanout_rgba(&mut mem).unwrap();
    assert_eq!(rgba0, vec![3, 2, 1, 0xff]);

    // LO-only write must not expose a torn address to readback; keep using fb0 until HI commit.
    dev.mmio_write(&mut mem, mmio::SCANOUT_FB_LO, 4, fb1 as u32);
    let rgba_after_lo = dev.read_scanout_rgba(&mut mem).unwrap();
    assert_eq!(rgba_after_lo, vec![3, 2, 1, 0xff]);

    // Commit the new address by writing HI.
    dev.mmio_write(&mut mem, mmio::SCANOUT_FB_HI, 4, (fb1 >> 32) as u32);
    let rgba1 = dev.read_scanout_rgba(&mut mem).unwrap();
    assert_eq!(rgba1, vec![30, 20, 10, 0xff]);
}

#[test]
fn vblank_tick_updates_counters_and_latches_irq_status() {
    let cfg = AeroGpuLegacyDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(cfg);

    const IRQ_SCANOUT_VBLANK: u32 = 1 << 1;

    dev.mmio_write(&mut mem, mmio::SCANOUT_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, IRQ_SCANOUT_VBLANK);

    let t0 = Instant::now();
    dev.tick(t0);
    assert_eq!(
        dev.mmio_read(&mut mem, mmio::IRQ_STATUS, 4) & IRQ_SCANOUT_VBLANK,
        0
    );
    assert_eq!(dev.mmio_read(&mut mem, mmio::SCANOUT0_VBLANK_SEQ_LO, 4), 0);

    dev.tick(t0 + Duration::from_millis(100));
    assert_ne!(
        dev.mmio_read(&mut mem, mmio::IRQ_STATUS, 4) & IRQ_SCANOUT_VBLANK,
        0
    );
    assert!(dev.irq_level());

    dev.mmio_write(&mut mem, mmio::IRQ_ACK, 4, IRQ_SCANOUT_VBLANK);
    assert_eq!(
        dev.mmio_read(&mut mem, mmio::IRQ_STATUS, 4) & IRQ_SCANOUT_VBLANK,
        0
    );
    assert!(!dev.irq_level());

    dev.tick(t0 + Duration::from_millis(200));
    assert_ne!(
        dev.mmio_read(&mut mem, mmio::IRQ_STATUS, 4) & IRQ_SCANOUT_VBLANK,
        0
    );
    assert!(dev.irq_level());

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, 0);
    assert_eq!(
        dev.mmio_read(&mut mem, mmio::IRQ_STATUS, 4) & IRQ_SCANOUT_VBLANK,
        0
    );
    assert!(!dev.irq_level());

    let seq = (dev.mmio_read(&mut mem, mmio::SCANOUT0_VBLANK_SEQ_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::SCANOUT0_VBLANK_SEQ_HI, 4) as u64) << 32);
    assert_ne!(seq, 0);

    let t_ns = (dev.mmio_read(&mut mem, mmio::SCANOUT0_VBLANK_TIME_NS_LO, 4) as u64)
        | ((dev.mmio_read(&mut mem, mmio::SCANOUT0_VBLANK_TIME_NS_HI, 4) as u64) << 32);
    assert_ne!(t_ns, 0);
}

#[test]
fn features_regs_advertise_vblank_when_enabled() {
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(AeroGpuLegacyDeviceConfig::default());

    let features_lo = dev.mmio_read(&mut mem, proto::AEROGPU_MMIO_REG_FEATURES_LO as u64, 4) as u64;
    let features_hi = dev.mmio_read(&mut mem, proto::AEROGPU_MMIO_REG_FEATURES_HI as u64, 4) as u64;
    let features = features_lo | (features_hi << 32);

    assert_ne!(features & proto::AEROGPU_FEATURE_VBLANK, 0);

    let expected_period_ns: u32 = 1_000_000_000u64.div_ceil(60) as u32;
    let period = dev.mmio_read(
        &mut mem,
        proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64,
        4,
    );
    assert_eq!(period, expected_period_ns);
}

#[test]
fn features_regs_clear_vblank_when_disabled() {
    let mut mem = VecMemory::new(0x1000);
    let cfg = AeroGpuLegacyDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };
    let mut dev = new_test_device(cfg);

    let features_lo = dev.mmio_read(&mut mem, proto::AEROGPU_MMIO_REG_FEATURES_LO as u64, 4) as u64;
    let features_hi = dev.mmio_read(&mut mem, proto::AEROGPU_MMIO_REG_FEATURES_HI as u64, 4) as u64;
    let features = features_lo | (features_hi << 32);

    assert_eq!(features & proto::AEROGPU_FEATURE_VBLANK, 0);
    assert_eq!(
        dev.mmio_read(
            &mut mem,
            proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64,
            4
        ),
        0
    );
}
