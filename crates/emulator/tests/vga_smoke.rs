use emulator::devices::vga::{DisplayOutput, PortIO, VgaDevice};

#[test]
fn vga_mode13h_renders_via_canonical_device() {
    let mut dev = VgaDevice::new();
    dev.set_mode_13h();

    // Write a single pixel index and render.
    dev.mem_write_u8(0xA0000, 1);
    dev.present();

    assert_eq!(dev.get_resolution(), (320, 200));
    assert_eq!(dev.get_framebuffer()[0], 0xFFAA_0000);
}

#[test]
fn bochs_vbe_lfb_write_renders() {
    let mut dev = VgaDevice::new();

    // Program 64x64x32bpp, LFB enabled (Bochs VBE_DISPI).
    dev.port_write(0x01CE, 2, 0x0001);
    dev.port_write(0x01CF, 2, 64);
    dev.port_write(0x01CE, 2, 0x0002);
    dev.port_write(0x01CF, 2, 64);
    dev.port_write(0x01CE, 2, 0x0003);
    dev.port_write(0x01CF, 2, 32);
    dev.port_write(0x01CE, 2, 0x0004);
    dev.port_write(0x01CF, 2, 0x0041);

    // Write a single red pixel at (0,0) in BGRX format.
    let lfb_base = dev.lfb_base();
    dev.mem_write_u8(lfb_base, 0x00); // B
    dev.mem_write_u8(lfb_base.wrapping_add(1), 0x00); // G
    dev.mem_write_u8(lfb_base.wrapping_add(2), 0xFF); // R
    dev.mem_write_u8(lfb_base.wrapping_add(3), 0x00); // X

    dev.present();
    assert_eq!(dev.get_resolution(), (64, 64));
    assert_eq!(dev.get_framebuffer()[0], 0xFF00_00FF);
}
