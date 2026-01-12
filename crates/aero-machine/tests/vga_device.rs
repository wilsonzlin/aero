use aero_devices::a20_gate::A20_GATE_PORT;
use aero_gpu_vga::SVGA_LFB_BASE;
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn program_vbe_linear_64x64x32(m: &mut Machine) {
    // Bochs VBE_DISPI programming via 0x01CE/0x01CF index/data ports.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64); // XRES

    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64); // YRES

    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32); // BPP

    m.io_write(0x01CE, 2, 0x0004);
    // ENABLE | LFB_ENABLE.
    m.io_write(0x01CF, 2, 0x0041);
}

fn write_pixel_bgrx(m: &mut Machine, width: u32, x: u32, y: u32, b: u8, g: u8, r: u8) {
    let off = (y * width + x) * 4;
    let base = u64::from(SVGA_LFB_BASE) + u64::from(off);
    m.write_physical_u8(base, b);
    m.write_physical_u8(base + 1, g);
    m.write_physical_u8(base + 2, r);
    m.write_physical_u8(base + 3, 0);
}

#[test]
fn vga_vbe_linear_framebuffer_scanout_matches_expected_pixels() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_vga: true,
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    enable_a20(&mut m);
    program_vbe_linear_64x64x32(&mut m);

    // Pixel format is BGRX in guest memory, but exposed as RGBA8888 in `u32`.
    write_pixel_bgrx(&mut m, 64, 0, 0, 0x00, 0x00, 0xFF); // red
    write_pixel_bgrx(&mut m, 64, 1, 0, 0x00, 0xFF, 0x00); // green
    write_pixel_bgrx(&mut m, 64, 0, 1, 0xFF, 0x00, 0x00); // blue

    m.display_present();

    assert_eq!(m.display_resolution(), (64, 64));
    let fb = m.display_framebuffer();
    assert_eq!(fb[0], 0xFF00_00FF);
    assert_eq!(fb[1], 0xFF00_FF00);
    assert_eq!(fb[64], 0xFFFF_0000);
}

#[test]
fn vga_snapshot_roundtrip_preserves_scanout() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_vga: true,
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();
    enable_a20(&mut m);
    program_vbe_linear_64x64x32(&mut m);
    write_pixel_bgrx(&mut m, 64, 0, 0, 0x00, 0x00, 0xFF);
    write_pixel_bgrx(&mut m, 64, 1, 0, 0x00, 0xFF, 0x00);
    write_pixel_bgrx(&mut m, 64, 0, 1, 0xFF, 0x00, 0x00);
    m.display_present();
    let expected_res = m.display_resolution();
    let expected_fb: Vec<u32> = m.display_framebuffer().to_vec();

    let snap = m.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
    restored.display_present();

    assert_eq!(restored.display_resolution(), expected_res);
    assert_eq!(restored.display_framebuffer(), expected_fb.as_slice());
}
