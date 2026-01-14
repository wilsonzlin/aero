use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn base_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test deterministic/minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    }
}

#[test]
fn aerogpu_bochs_vbe_dispi_8bpp_display_present_uses_dac_palette() {
    let mut m = Machine::new(base_cfg()).unwrap();
    m.reset();

    // Program a 2x2x8bpp VBE mode via Bochs VBE_DISPI ports.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 2);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 2);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 8);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    // Use a fully-enabled PEL mask so palette indices are not masked away.
    m.io_write(0x3C6, 1, 0xFF);
    // Program palette entry 1 to pure red using classic VGA 6-bit values.
    m.io_write(0x3C8, 1, 0x01);
    m.io_write(0x3C9, 1, 63); // R
    m.io_write(0x3C9, 1, 0); // G
    m.io_write(0x3C9, 1, 0); // B

    // Write a single pixel with palette index 1 at the top-left of the VBE framebuffer.
    let base = m.vbe_lfb_base();
    m.write_physical_u8(base, 1);

    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));
    // RGBA8888 little-endian u32: [R, G, B, A].
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
