use aero_machine::{Machine, MachineConfig};

#[test]
fn vga_vbe_lfb_is_reachable_via_pci_mmio_router() {
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();

    // Match the programming sequence used by `aero-gpu-vga`'s
    // `vbe_linear_framebuffer_write_shows_up_in_output` test.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    // Always use the firmware-reported VBE PhysBasePtr so this test stays robust if the LFB base
    // changes (e.g. standalone VGA stub vs AeroGPU BAR1-backed legacy VBE).
    let base = m.vbe_lfb_base();
    // Write a red pixel at (0,0) in packed 32bpp BGRX via *machine memory*.
    m.write_physical_u32(u64::from(base), 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
