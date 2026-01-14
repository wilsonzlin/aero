use aero_gpu_vga::{VBE_DISPI_DATA_PORT, VBE_DISPI_INDEX_PORT};
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
