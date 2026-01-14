use std::sync::Arc;

use aero_gpu_vga::VBE_DISPI_INDEX_PORT;
use aero_shared::scanout_state::{
    ScanoutState, SCANOUT_FORMAT_B5G5R5A1, SCANOUT_FORMAT_B5G6R5, SCANOUT_FORMAT_B8G8R8A8,
    SCANOUT_FORMAT_B8G8R8A8_SRGB, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_FORMAT_B8G8R8X8_SRGB,
    SCANOUT_FORMAT_R8G8B8A8, SCANOUT_FORMAT_R8G8B8A8_SRGB, SCANOUT_FORMAT_R8G8B8X8,
    SCANOUT_FORMAT_R8G8B8X8_SRGB, SCANOUT_SOURCE_WDDM,
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

    let cfg = AeroGpuDeviceConfig {
        vram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    dev.set_scanout_state(Some(scanout.clone()));
    // Enable PCI MMIO decode so BAR0 writes are accepted.
    dev.config_write(0x04, 2, (1 << 0) | (1 << 1));

    let gen0 = scanout.snapshot().generation;

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
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    // Transition ENABLE from 0->1, which should publish the scanout descriptor.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    let snap = scanout.snapshot();
    assert_eq!(snap.generation, gen0 + 1);
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr_lo, fb_gpa as u32);
    assert_eq!(snap.base_paddr_hi, (fb_gpa >> 32) as u32);
    assert_eq!(snap.width, 800);
    assert_eq!(snap.height, 600);
    assert_eq!(snap.pitch_bytes, 800 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);

    // Transition ENABLE from 1->0 and verify we publish a WDDM "disabled" descriptor (blank),
    // without reverting ownership back to legacy until reset.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 0);
    let snap2 = scanout.snapshot();
    assert_eq!(snap2.generation, gen0 + 2);
    assert_eq!(snap2.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap2.base_paddr(), 0);
    assert_eq!(snap2.width, 0);
    assert_eq!(snap2.height, 0);
    assert_eq!(snap2.pitch_bytes, 0);
    assert_eq!(snap2.format, SCANOUT_FORMAT_B8G8R8X8);

    // Legacy VGA/VBE activity must not steal scanout ownership once WDDM has claimed it.
    dev.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0);
    let snap3 = scanout.snapshot();
    assert_eq!(snap3.generation, gen0 + 2);
    assert_eq!(snap3.source, SCANOUT_SOURCE_WDDM);
}

#[test]
fn unsupported_scanout_format_after_claim_publishes_deterministic_disabled_descriptor() {
    let mut mem = DummyMemory;
    let scanout = Arc::new(ScanoutState::new());

    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0, 0);
    dev.set_scanout_state(Some(scanout.clone()));
    // Enable PCI MMIO decode so BAR0 writes are accepted.
    dev.config_write(0x04, 2, 1 << 1);

    let gen0 = scanout.snapshot().generation;

    // Program a valid WDDM scanout configuration and enable it so WDDM owns scanout.
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
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    // Transition ENABLE from 0->1, which should publish the scanout descriptor.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    let snap_claimed = scanout.snapshot();
    assert_eq!(snap_claimed.generation, gen0 + 1);
    assert_eq!(snap_claimed.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap_claimed.base_paddr(), fb_gpa);

    // Switch to an unsupported scanout format (e.g. depth/stencil). The device must publish a
    // deterministic disabled descriptor rather than publishing an unsupported pixel format value.
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::D24UnormS8Uint as u32,
    );

    let snap0 = scanout.snapshot();
    assert_eq!(snap0.generation, gen0 + 2);
    assert_eq!(snap0.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap0.base_paddr(), 0);
    assert_eq!(snap0.width, 0);
    assert_eq!(snap0.height, 0);
    assert_eq!(snap0.pitch_bytes, 0);
    assert_eq!(snap0.format, SCANOUT_FORMAT_B8G8R8X8);

    // Reprogramming another unsupported format should be deterministic and not publish a new
    // (different) descriptor.
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::D32Float as u32,
    );
    let snap1 = scanout.snapshot();
    assert_eq!(snap1.generation, gen0 + 2);
    assert_eq!(snap1.source, SCANOUT_SOURCE_WDDM);
}

#[test]
fn preserves_supported_scanout_formats_in_shared_state() {
    let mut mem = DummyMemory;
    let scanout = Arc::new(ScanoutState::new());

    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0, 0);
    dev.set_scanout_state(Some(scanout.clone()));
    // Enable PCI MMIO decode so BAR0 writes are accepted.
    dev.config_write(0x04, 2, 1 << 1);

    // Program scanout0 registers as a guest would.
    let fb_gpa: u64 = 0x1234_5678_9abc_def0;
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 800);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 600);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 800 * 4);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    // BGRA should be preserved (alpha-capable).
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8A8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    let snap = scanout.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8A8);

    // sRGB variants should preserve the sRGB discriminant in the shared state (layout-compatible).
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8X8UnormSrgb as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8_SRGB);

    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8A8UnormSrgb as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8A8_SRGB);

    // RGBA/RGBX should also be preserved (scanout consumer supports both channel orderings).
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::R8G8B8A8Unorm as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8A8);

    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::R8G8B8X8Unorm as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8X8);

    // sRGB RGBA/RGBX variants should preserve their discriminants as well.
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::R8G8B8A8UnormSrgb as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8A8_SRGB);

    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::R8G8B8X8UnormSrgb as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8X8_SRGB);

    // 16bpp formats should also be representable in the shared scanout descriptor.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 800 * 2);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B5G6R5Unorm as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_B5G6R5);

    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B5G5R5A1Unorm as u32,
    );
    let snap = scanout.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_B5G5R5A1);
}
