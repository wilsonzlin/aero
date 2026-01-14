use aero_devices::pci::profile::{AEROGPU_BAR1_VRAM_INDEX, AEROGPU_VRAM_SIZE};
use aero_devices::pci::PciDevice as _;
use aero_devices_gpu::{
    AeroGpuPciDevice, LEGACY_VGA_PADDR_BASE, LEGACY_VGA_PADDR_END, LEGACY_VGA_VRAM_BYTES,
    VBE_LFB_OFFSET,
};
use memory::MmioHandler as _;

#[test]
fn bar1_mmio_writes_back_device_vram() {
    let dev = AeroGpuPciDevice::default();
    let vram = dev.vram_shared();
    let mut bar1 = dev.bar1_mmio_handler();

    // Writes through BAR1 at offset 0 should land in VRAM[0].
    bar1.write(0, 4, 0xAABB_CCDD);
    assert_eq!(bar1.read(0, 4), 0xAABB_CCDD);

    let vram = vram.borrow();
    assert_eq!(&vram[0..4], &[0xDD, 0xCC, 0xBB, 0xAA]);
}

#[test]
fn bar1_mmio_out_of_range_reads_return_all_ones_and_writes_are_ignored() {
    let dev = AeroGpuPciDevice::default();
    let mut bar1 = dev.bar1_mmio_handler();

    // Writes past the end should not panic and should have no effect.
    bar1.write(AEROGPU_VRAM_SIZE, 1, 0x00);

    // Reads past the end return all ones (floating bus semantics).
    assert_eq!(bar1.read(AEROGPU_VRAM_SIZE, 1), 0xFF);
    assert_eq!(bar1.read(AEROGPU_VRAM_SIZE, 4), 0xFFFF_FFFF);
}

#[test]
fn bar1_mmio_rejects_sizes_larger_than_8_bytes() {
    let dev = AeroGpuPciDevice::default();
    let mut bar1 = dev.bar1_mmio_handler();

    // Read >8 bytes returns all ones.
    assert_eq!(bar1.read(0, 9), u64::MAX);

    // Write >8 bytes is ignored.
    bar1.write(0, 9, 0);
    assert_eq!(bar1.read(0, 1), 0);
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
fn legacy_vga_mmio_writes_alias_to_expected_vram_offsets() {
    let dev = AeroGpuPciDevice::default();
    let vram = dev.vram_shared();
    let mut legacy = dev.legacy_vga_mmio_handler();

    let off = AeroGpuPciDevice::legacy_vga_paddr_to_vram_offset(0xB8000).unwrap();
    legacy.write(off, 2, 0x1122);

    {
        let vram = vram.borrow();
        let off = off as usize;
        assert_eq!(vram[off], 0x22);
        assert_eq!(vram[off + 1], 0x11);
    }

    // The BAR1 view of VRAM should observe the same bytes.
    let mut bar1 = dev.bar1_mmio_handler();
    assert_eq!(bar1.read(off, 2), 0x1122);
}

#[test]
fn legacy_vga_alias_checks_window_bounds() {
    assert_eq!(
        AeroGpuPciDevice::legacy_vga_paddr_to_vram_offset(LEGACY_VGA_PADDR_BASE),
        Some(0)
    );
    assert_eq!(
        AeroGpuPciDevice::legacy_vga_paddr_to_vram_offset(LEGACY_VGA_PADDR_END - 1),
        Some(LEGACY_VGA_PADDR_END - LEGACY_VGA_PADDR_BASE - 1)
    );
    assert!(AeroGpuPciDevice::legacy_vga_paddr_to_vram_offset(LEGACY_VGA_PADDR_END).is_none());
    assert!(AeroGpuPciDevice::legacy_vga_paddr_to_vram_offset(LEGACY_VGA_PADDR_BASE - 1).is_none());
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
    // Layout contract for `aero-devices-gpu` BAR1 mapping helpers:
    // - guest-visible legacy VGA alias aperture is 128KiB (0xA0000..0xBFFFF).
    // - the VBE LFB begins after the full 4-plane VGA planar region at 0x40000.
    assert_eq!(LEGACY_VGA_VRAM_BYTES, 0x20_000);
    assert_eq!(VBE_LFB_OFFSET, 0x40_000);
    // The BIOS VBE linear framebuffer base must be 64KiB-aligned.
    assert_eq!(VBE_LFB_OFFSET % 0x1_0000, 0);
}

#[test]
fn vram_allocation_matches_bar1_definition() {
    let dev = AeroGpuPciDevice::default();

    let bar1_size = dev
        .config()
        .bar_range(AEROGPU_BAR1_VRAM_INDEX)
        .expect("AeroGPU profile should define BAR1")
        .size;
    assert_eq!(bar1_size, AEROGPU_VRAM_SIZE);

    let vram_len = dev.vram_shared().borrow().len() as u64;
    assert!(vram_len > 0);
    assert!(vram_len <= bar1_size);

    // On native targets the backing store is sized to the full BAR1 aperture.
    #[cfg(not(target_arch = "wasm32"))]
    assert_eq!(vram_len, bar1_size);
}
