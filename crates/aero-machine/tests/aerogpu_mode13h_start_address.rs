use aero_machine::{Machine, MachineConfig, RunExit, ScanoutSource};
use pretty_assertions::assert_eq;

fn build_int10_set_mode13h_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x0013 (set video mode 13h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x13, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn aerogpu_mode13h_respects_crtc_start_address_and_byte_mode() {
    // Keep the machine minimal/deterministic for a unit test.
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let boot = build_int10_set_mode13h_boot_sector();
    let mut m = Machine::new(cfg.clone()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyVga);

    // Populate the start of the VGA graphics window with a simple ramp.
    for i in 0..32u8 {
        m.write_physical_u8(0xA0000 + u64::from(i), i);
    }

    // Default CRTC byte mode is off; start address is interpreted as a word offset, so start=1
    // shifts by 2 bytes (2 pixels).
    m.io_write(0x3D4, 1, 0x0C);
    m.io_write(0x3D5, 1, 0x00);
    m.io_write(0x3D4, 1, 0x0D);
    m.io_write(0x3D5, 1, 0x01);

    m.display_present();
    assert_eq!(m.display_resolution(), (320, 200));
    // VGA palette entry 2 is EGA green (0x00,0xAA,0x00).
    assert_eq!(m.display_framebuffer()[0], 0xFF00_AA00);

    // Enable CRTC byte mode (index 0x17 bit 6); now start=1 shifts by 1 byte (1 pixel).
    m.io_write(0x3D4, 1, 0x17);
    m.io_write(0x3D5, 1, 0x40);

    m.display_present();
    // VGA palette entry 1 is EGA blue (0x00,0x00,0xAA).
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_0000);

    // Snapshot/restore should preserve both the VRAM contents and the CRTC register state that
    // drives mode 13h panning.
    let snap = m.take_snapshot_full().unwrap();

    let boot = build_int10_set_mode13h_boot_sector();
    let mut m2 = Machine::new(cfg).unwrap();
    m2.set_disk_image(boot.to_vec()).unwrap();
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    assert_eq!(m2.active_scanout_source(), ScanoutSource::LegacyVga);
    m2.display_present();
    assert_eq!(m2.display_resolution(), (320, 200));
    assert_eq!(m2.display_framebuffer()[0], 0xFFAA_0000);

    // Validate the CRTC regs survived restore too.
    m2.io_write(0x3D4, 1, 0x0C);
    assert_eq!(m2.io_read(0x3D5, 1) as u8, 0x00);
    m2.io_write(0x3D4, 1, 0x0D);
    assert_eq!(m2.io_read(0x3D5, 1) as u8, 0x01);
    m2.io_write(0x3D4, 1, 0x17);
    assert_eq!(m2.io_read(0x3D5, 1) as u8, 0x40);
}
