use std::sync::Arc;

use aero_shared::scanout_state::{ScanoutState, SCANOUT_SOURCE_WDDM};
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

fn new_test_device(scanout_state: Arc<ScanoutState>) -> AeroGpuPciDevice {
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);
    dev.set_scanout_state(Some(scanout_state));
    // Enable PCI MMIO decode + bus mastering so register writes behave like a real enumerated device.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev
}

#[test]
fn scanout_state_updates_on_fb_gpa_flips_while_enabled() {
    let mut mem = DummyMemory;
    let scanout_state = Arc::new(ScanoutState::new());
    let mut dev = new_test_device(scanout_state.clone());

    // Enable scanout0 (WDDM).
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    let snap0 = scanout_state.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap0.base_paddr(), 0);

    // Provide a minimally valid scanout descriptor so subsequent FB flips are considered valid.
    // Real drivers program these registers before (and sometimes while) scanout is enabled.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 4);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8Unorm as u32,
    );

    // Flip to a new framebuffer address. Drivers typically write LO then HI.
    let fb1 = 0x1234_5678_9abc_def0u64;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb1 as u32);

    // Must not publish a torn 64-bit update after only the LO write.
    let snap_after_lo = scanout_state.snapshot();
    assert_eq!(snap_after_lo.base_paddr(), 0);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb1 >> 32) as u32);
    let snap1 = scanout_state.snapshot();
    assert_eq!(snap1.base_paddr(), fb1);

    // Flip again.
    let fb2 = 0x0fed_cba9_8765_4321u64;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb2 as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb2 >> 32) as u32);
    let snap2 = scanout_state.snapshot();
    assert_eq!(snap2.base_paddr(), fb2);
}
