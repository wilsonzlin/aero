use std::sync::Arc;

use aero_shared::scanout_state::{
    ScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_WDDM,
};
use emulator::devices::aerogpu_regs::mmio;
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::io::pci::{MmioDevice, PciDevice};
use memory::MemoryBus;

struct DummyMemory;

impl MemoryBus for DummyMemory {
    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        buf.fill(0);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
}

#[test]
fn publishes_wddm_scanout_state_on_enable_transition() {
    let mut mem = DummyMemory;
    let scanout = Arc::new(ScanoutState::new());

    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);
    dev.set_scanout_state(Some(scanout.clone()));
    // Enable PCI MMIO decode so BAR0 writes are accepted.
    dev.config_write(0x04, 2, 1 << 1);

    // Program scanout0 registers as a guest would.
    let fb_gpa: u64 = 0x1234_5678_9abc_def0;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 800);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 600);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 800 * 4);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FB_GPA_HI,
        4,
        (fb_gpa >> 32) as u32,
    );

    // Transition ENABLE from 0->1, which should publish the scanout descriptor.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    let snap = scanout.snapshot();
    assert_eq!(snap.generation, 1);
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr_lo, fb_gpa as u32);
    assert_eq!(snap.base_paddr_hi, (fb_gpa >> 32) as u32);
    assert_eq!(snap.width, 800);
    assert_eq!(snap.height, 600);
    assert_eq!(snap.pitch_bytes, 800 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);

    // Transition ENABLE from 1->0 and verify we fall back to legacy.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 0);
    let snap2 = scanout.snapshot();
    assert_eq!(snap2.generation, 2);
    assert_eq!(snap2.source, SCANOUT_SOURCE_LEGACY_TEXT);
    assert_eq!(snap2.width, 0);
    assert_eq!(snap2.height, 0);
}

