use aero_devices::pci::profile::AEROGPU_VRAM_SIZE;
use aero_devices_gpu::{
    AeroGpuPciDevice, LEGACY_VGA_PADDR_BASE, LEGACY_VGA_VRAM_BYTES, VBE_LFB_OFFSET,
};
use memory::MmioHandler as _;

#[test]
fn bar1_mmio_read_write_roundtrip() {
    let dev = AeroGpuPciDevice::default();
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

#[test]
fn vbe_lfb_offset_matches_machine_vga_vram_layout_contract() {
    // `aero_machine` reserves 256KiB for legacy VGA planar storage (4 × 64KiB planes) and starts
    // the VBE linear framebuffer after that region.
    assert_eq!(VBE_LFB_OFFSET, 0x40_000);
}

#[test]
fn vbe_lfb_alias_maps_lfb_base_to_expected_vram_offset() {
    let bar1_base = 0x4000_0000u64;
    let paddr = bar1_base + VBE_LFB_OFFSET;
    let off = AeroGpuPciDevice::vbe_lfb_paddr_to_vram_offset(bar1_base, paddr).unwrap();
    assert_eq!(off, VBE_LFB_OFFSET);
}

#[test]
fn vbe_lfb_alias_rejects_paddrs_before_lfb_base() {
    let bar1_base = 0x4000_0000u64;
    let paddr = bar1_base + VBE_LFB_OFFSET - 1;
    assert!(AeroGpuPciDevice::vbe_lfb_paddr_to_vram_offset(bar1_base, paddr).is_none());
}

#[test]
fn vbe_lfb_alias_rejects_paddrs_past_bar1_end() {
    let bar1_base = 0x4000_0000u64;
    let paddr = bar1_base + AEROGPU_VRAM_SIZE;
    assert!(AeroGpuPciDevice::vbe_lfb_paddr_to_vram_offset(bar1_base, paddr).is_none());
}

#[test]
fn bar1_layout_constants_match_canonical_vga_vbe_layout() {
    // Canonical layout (see `docs/16-aerogpu-vga-vesa-compat.md`):
    // - guest-visible legacy VGA alias aperture is 128KiB (0xA0000..0xBFFFF).
    // - the VRAM backing reserve at the start of BAR1 is 256KiB (4×64KiB planes), so the VBE LFB
    //   begins at 0x40000.
    assert_eq!(LEGACY_VGA_VRAM_BYTES, 0x20_000);
    assert_eq!(VBE_LFB_OFFSET, 0x40_000);
}
