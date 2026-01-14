use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::io::pci::PciDevice as _;

#[test]
fn bar1_vram_mmio_read_does_not_wrap_on_offset_overflow() {
    let cfg = AeroGpuDeviceConfig {
        vram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    // Enable PCI MMIO decode so BAR1 reads are accepted.
    dev.config_write(0x04, 2, 1 << 1);

    // Seed VRAM[0..2] with known bytes so we can detect accidental wraparound.
    dev.vram_mmio_write(0, 2, 0xBBAA);
    assert_eq!(dev.vram_mmio_read(0, 2), 0xBBAA);

    // Reads with overflowing offsets must not wrap around and leak VRAM[0..].
    assert_eq!(dev.vram_mmio_read(u64::MAX - 1, 4), u64::from(u32::MAX));
}

#[test]
fn bar1_vram_mmio_write_does_not_wrap_on_offset_overflow() {
    let cfg = AeroGpuDeviceConfig {
        vram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    // Enable PCI MMIO decode so BAR1 writes are accepted.
    dev.config_write(0x04, 2, 1 << 1);

    // Seed VRAM[0..2] with known bytes so we can detect accidental wraparound.
    dev.vram_mmio_write(0, 2, 0xBBAA);

    // Writes with overflowing offsets must not wrap around and scribble VRAM[0..].
    dev.vram_mmio_write(u64::MAX - 1, 4, 0x1122_3344);

    assert_eq!(dev.vram_mmio_read(0, 2), 0xBBAA);
}
