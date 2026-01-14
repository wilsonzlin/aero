use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn base_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    }
}

fn program_vbe_linear_64x64x32_vbe_dispi(m: &mut Machine) {
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

#[test]
fn aerogpu_snapshot_preserves_vbe_dispi_register_file_and_scanout() {
    let cfg = base_cfg();
    let mut m = Machine::new(cfg.clone()).unwrap();

    program_vbe_linear_64x64x32_vbe_dispi(&mut m);

    // Write a red pixel at (0,0) in packed 32bpp BGRX.
    let base = m.vbe_lfb_base();
    m.write_physical_u32(base, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    let expected_fb: Vec<u32> = m.display_framebuffer().to_vec();

    let snap = m.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.reset();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Ensure the Bochs VBE_DISPI register file survives snapshot/restore.
    restored.io_write(0x01CE, 2, 0x0001);
    assert_eq!(restored.io_read(0x01CF, 2) as u16, 64);
    restored.io_write(0x01CE, 2, 0x0002);
    assert_eq!(restored.io_read(0x01CF, 2) as u16, 64);
    restored.io_write(0x01CE, 2, 0x0003);
    assert_eq!(restored.io_read(0x01CF, 2) as u16, 32);
    restored.io_write(0x01CE, 2, 0x0004);
    assert_eq!(restored.io_read(0x01CF, 2) as u16, 0x0041);

    restored.display_present();
    assert_eq!(restored.display_resolution(), (64, 64));
    assert_eq!(restored.display_framebuffer(), expected_fb.as_slice());
}

