use aero_gpu_vga::{VBE_DISPI_DATA_PORT, VBE_DISPI_INDEX_PORT, VBE_FRAMEBUFFER_OFFSET};
use aero_machine::{Machine, MachineConfig};

fn program_vbe_linear_64x64x32(m: &mut Machine) {
    // Match the programming sequence used by `aero-gpu-vga`'s
    // `vbe_linear_framebuffer_write_shows_up_in_output` test.
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0001);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 64);
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0002);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 64);
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0003);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 32);
    m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0004);
    m.io_write(VBE_DISPI_DATA_PORT, 2, 0x0041);
}

#[test]
fn vga_vbe_lfb_is_reachable_via_direct_mmio_without_pc_platform() {
    // Use a non-default base to ensure there are no hidden dependencies on
    // `aero_gpu_vga::SVGA_LFB_BASE` in the non-PC-platform MMIO wiring path.
    let lfb_base: u32 = 0xD000_0000;

    // Keep the test output deterministic (not required for correctness, but avoids noise if the
    // test ever gets extended).
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(lfb_base),
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();

    assert_eq!(m.vbe_lfb_base(), u64::from(lfb_base));
    program_vbe_linear_64x64x32(&mut m);

    let base = m.vbe_lfb_base();
    assert_eq!(base, u64::from(lfb_base));
    // Write a red pixel at (0,0) in BGRX format via *machine memory*.
    m.write_physical_u8(base, 0x00); // B
    m.write_physical_u8(base + 1, 0x00); // G
    m.write_physical_u8(base + 2, 0xFF); // R
    m.write_physical_u8(base + 3, 0x00); // X

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);

    // Ensure the LFB mapping remains reachable across resets (`map_mmio_once` persists mappings).
    m.reset();
    assert_eq!(m.vbe_lfb_base(), u64::from(lfb_base));
    program_vbe_linear_64x64x32(&mut m);
    let base2 = m.vbe_lfb_base();
    assert_eq!(base2, u64::from(lfb_base));
    m.write_physical_u8(base2 + 4, 0x00); // B
    m.write_physical_u8(base2 + 5, 0xFF); // G
    m.write_physical_u8(base2 + 6, 0x00); // R
    m.write_physical_u8(base2 + 7, 0x00); // X

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[1], 0xFF00_FF00);
}

#[test]
fn vga_vbe_lfb_base_can_be_derived_from_vram_bar_base_and_lfb_offset_without_pc_platform() {
    // Exercise the optional VRAM layout knobs even when the PC platform is disabled (direct MMIO
    // wiring): the effective LFB base should be:
    //
    //   lfb_base = vga_vram_bar_base + vga_lfb_offset
    //
    // (and then masked down to the VRAM aperture size alignment).
    //
    // Use the standardized non-default base to ensure there are no hidden dependencies on
    // `aero_gpu_vga::SVGA_LFB_BASE`.
    let requested_lfb_base: u32 = 0xD000_0000;
    let lfb_offset: u32 = VBE_FRAMEBUFFER_OFFSET as u32;
    let requested_vram_bar_base: u32 = requested_lfb_base.wrapping_sub(lfb_offset);

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        vga_vram_bar_base: Some(requested_vram_bar_base),
        vga_lfb_offset: Some(lfb_offset),
        // Keep the test deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    let vga = m.vga().expect("VGA enabled");
    let bar_size_bytes: u32 = vga
        .borrow()
        .vram_size()
        .try_into()
        .expect("VRAM size fits in u32");
    assert!(bar_size_bytes.is_power_of_two());

    let derived_lfb_base = requested_vram_bar_base.wrapping_add(lfb_offset);
    assert_eq!(derived_lfb_base, requested_lfb_base);
    let expected_aligned_base = derived_lfb_base & !(bar_size_bytes - 1);
    assert_eq!(m.vbe_lfb_base_u32(), expected_aligned_base);

    program_vbe_linear_64x64x32(&mut m);

    // Write a red pixel at (0,0) in packed 32bpp BGRX via *machine memory*.
    m.write_physical_u32(u64::from(expected_aligned_base), 0x00FF_0000);
    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
