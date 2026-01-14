use aero_machine::{Machine, MachineConfig, RunExit, ScanoutSource};
use pretty_assertions::assert_eq;

fn build_mode13h_write_pixel_boot_sector(x: u16, y: u16, color: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x0013 (set video mode 13h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x13, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // xor bx, bx (page 0)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;

    // mov cx, x
    sector[i..i + 3].copy_from_slice(&[0xB9, (x & 0xFF) as u8, (x >> 8) as u8]);
    i += 3;
    // mov dx, y
    sector[i..i + 3].copy_from_slice(&[0xBA, (y & 0xFF) as u8, (y >> 8) as u8]);
    i += 3;

    // mov ax, 0x0C?? (write graphics pixel; AH=0x0C, AL=color)
    sector[i..i + 3].copy_from_slice(&[0xB8, color, 0x0C]);
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
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

fn deterministic_aerogpu_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep test output deterministic.
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
fn aerogpu_snapshot_preserves_mode13h_scanout() {
    let x = 10u16;
    let y = 20u16;
    let color = 4u8; // EGA red in the default VGA palette.
    let boot = build_mode13h_write_pixel_boot_sector(x, y, color);

    let cfg = deterministic_aerogpu_cfg();
    let mut m = Machine::new(cfg.clone()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyVga);
    assert_eq!(m.display_resolution(), (320, 200));
    let pixel_before = m.display_framebuffer()[(y as usize) * 320 + (x as usize)];
    assert_eq!(pixel_before, 0xFF00_00AA);

    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = Machine::new(cfg).unwrap();
    m2.set_disk_image(boot.to_vec()).unwrap();
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    m2.display_present();
    assert_eq!(m2.active_scanout_source(), ScanoutSource::LegacyVga);
    assert_eq!(m2.display_resolution(), (320, 200));
    let pixel_after = m2.display_framebuffer()[(y as usize) * 320 + (x as usize)];
    assert_eq!(pixel_after, pixel_before);
}
