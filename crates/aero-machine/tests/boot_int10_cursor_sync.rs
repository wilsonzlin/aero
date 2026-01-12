use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bda::BDA_SCREEN_COLS_ADDR;

fn build_int10_set_cursor_pos_boot_sector(row: u8, col: u8) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ah, 0x02  ; INT 10h AH=02h Set Cursor Position
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x02]);
    i += 2;
    // mov bh, 0x00  ; page 0
    sector[i..i + 2].copy_from_slice(&[0xB7, 0x00]);
    i += 2;
    // mov dh, row
    sector[i..i + 2].copy_from_slice(&[0xB6, row]);
    i += 2;
    // mov dl, col
    sector[i..i + 2].copy_from_slice(&[0xB2, col]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;
    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn build_int10_set_cursor_shape_boot_sector(start: u8, end: u8) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ah, 0x01  ; INT 10h AH=01h Set Cursor Shape
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x01]);
    i += 2;
    // mov ch, start
    sector[i..i + 2].copy_from_slice(&[0xB5, start]);
    i += 2;
    // mov cl, end
    sector[i..i + 2].copy_from_slice(&[0xB1, end]);
    i += 2;
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
    let mut halted = false;
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => {
                halted = true;
                break;
            }
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");
}

fn read_crtc_cursor_regs(m: &mut Machine) -> (u8, u8, u16) {
    m.io_write(0x3D4, 1, 0x0A);
    let start = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0B);
    let end = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0E);
    let hi = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0F);
    let lo = m.io_read(0x3D5, 1) as u8;
    (start, end, ((hi as u16) << 8) | (lo as u16))
}

#[test]
fn int10_cursor_updates_sync_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        ..Default::default()
    })
    .unwrap();

    let boot = build_int10_set_cursor_pos_boot_sector(5, 10);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Cursor shape is initialized by BIOS POST, but our HLE BIOS does not perform VGA port I/O.
    // Ensure the machine syncs BDA cursor state into VGA CRTC regs after POST.
    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos, 0);

    run_until_halt(&mut m);

    let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
    let expected_pos = 5u16.saturating_mul(cols) + 10u16;

    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos, expected_pos);
}

#[test]
fn int10_cursor_shape_updates_sync_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        ..Default::default()
    })
    .unwrap();

    // Hide the cursor using CH bit5 (cursor disable).
    let boot = build_int10_set_cursor_shape_boot_sector(0x20, 0x07);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    let (start, end, _pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x20);
    assert_eq!(end, 0x07);
}
