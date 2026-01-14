use aero_machine::{Machine, MachineConfig, RunExit};
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
fn boot_sector_int10_aerogpu_mode13h_write_pixel_is_visible() {
    let x = 10u16;
    let y = 20u16;
    let color = 4u8; // EGA red (0xAA,0x00,0x00) in the default VGA palette.
    let boot = build_mode13h_write_pixel_boot_sector(x, y, color);

    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    m.display_present();
    assert_eq!(m.display_resolution(), (320, 200));
    let fb = m.display_framebuffer();
    let pixel = fb[(y as usize) * 320 + (x as usize)];
    // Default VGA palette entry 4 is EGA red (0xAA,0x00,0x00).
    assert_eq!(pixel, 0xFF00_00AA);
}
