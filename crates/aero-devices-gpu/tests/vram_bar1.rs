use aero_devices_gpu::{AeroGpuPciDevice, LEGACY_VGA_PADDR_BASE};
use memory::MmioHandler as _;

#[test]
fn bar1_mmio_read_write_roundtrip() {
    let dev = AeroGpuPciDevice::new();
    let mut bar1 = dev.bar1_mmio_handler();

    bar1.write(0x1234, 4, 0xAABB_CCDD);
    assert_eq!(bar1.read(0x1234, 4), 0xAABB_CCDD);

    // Ensure byte granularity matches little-endian layout.
    assert_eq!(bar1.read(0x1234, 1), 0xDD);
    assert_eq!(bar1.read(0x1235, 1), 0xCC);
}

#[test]
fn legacy_vga_alias_maps_text_buffer_to_expected_offset() {
    // Text mode memory lives at 0xB8000. In the legacy VGA window (0xA0000..0xC0000), that should
    // map to an offset of 0x18000 from the window base (0xA0000).
    let off = AeroGpuPciDevice::legacy_vga_paddr_to_vram_offset(0xB8000).unwrap();
    assert_eq!(off, 0xB8000 - LEGACY_VGA_PADDR_BASE);
    assert_eq!(off, 0x18_000);
}

