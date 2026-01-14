use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bda::{
    BDA_ACTIVE_PAGE_ADDR, BDA_CURSOR_POS_PAGE0_ADDR, BDA_PAGE_SIZE_ADDR, BDA_SCREEN_COLS_ADDR,
};

fn build_int10_set_cursor_pos_boot_sector(row: u8, col: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
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

fn build_int10_set_cursor_pos_on_page_boot_sector(page: u8, row: u8, col: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ah, 0x02  ; INT 10h AH=02h Set Cursor Position
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x02]);
    i += 2;
    // mov bh, page
    sector[i..i + 2].copy_from_slice(&[0xB7, page]);
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

fn build_int10_set_cursor_shape_boot_sector(start: u8, end: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
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

fn build_int10_set_active_page_boot_sector(page: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ah, 0x05  ; INT 10h AH=05h Select Active Display Page
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x05]);
    i += 2;
    // mov al, page
    sector[i..i + 2].copy_from_slice(&[0xB0, page]);
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

fn build_int10_set_cursor_pos_on_page_then_select_active_page_boot_sector(
    cursor_page: u8,
    cursor_row: u8,
    cursor_col: u8,
    active_page: u8,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ah, 0x02  ; INT 10h AH=02h Set Cursor Position
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x02]);
    i += 2;
    // mov bh, cursor_page
    sector[i..i + 2].copy_from_slice(&[0xB7, cursor_page]);
    i += 2;
    // mov dh, cursor_row
    sector[i..i + 2].copy_from_slice(&[0xB6, cursor_row]);
    i += 2;
    // mov dl, cursor_col
    sector[i..i + 2].copy_from_slice(&[0xB2, cursor_col]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ah, 0x05  ; INT 10h AH=05h Select Active Display Page
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x05]);
    i += 2;
    // mov al, active_page
    sector[i..i + 2].copy_from_slice(&[0xB0, active_page]);
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

fn build_int10_teletype_boot_sector(ch: u8, attr: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ah, 0x0E  ; INT 10h AH=0Eh Teletype output
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x0E]);
    i += 2;
    // mov al, ch
    sector[i..i + 2].copy_from_slice(&[0xB0, ch]);
    i += 2;
    // mov bh, 0x00  ; page 0
    sector[i..i + 2].copy_from_slice(&[0xB7, 0x00]);
    i += 2;
    // mov bl, attr
    sector[i..i + 2].copy_from_slice(&[0xB3, attr]);
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

fn build_int10_write_string_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // Place the string near the end of the boot sector so it's well away from the code stream.
    // The BIOS loads the boot sector to physical 0x7C00 and starts executing at 0000:7C00, so
    // ES:BP can use an absolute offset into 0x0000 segment space.
    let text = b"ABC";
    let text_offset = 0x01E0usize;
    let text_addr = 0x7C00u16 + text_offset as u16;

    sector[text_offset..text_offset + text.len()].copy_from_slice(text);

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov bp, text_addr
    sector[i..i + 3].copy_from_slice(&[0xBD, (text_addr & 0x00FF) as u8, (text_addr >> 8) as u8]);
    i += 3;

    // mov ax, 0x1301 ; INT 10h AH=13h Write String, AL=01h (update cursor, no inline attrs)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x13]);
    i += 3;
    // mov bx, 0x001F ; BH=0 page, BL=0x1F attribute (white on blue)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x1F, 0x00]);
    i += 3;
    // mov cx, 3 ; string length
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x03, 0x00]);
    i += 3;
    // xor dx, dx ; DH=row0, DL=col0
    sector[i..i + 2].copy_from_slice(&[0x31, 0xD2]);
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

fn build_set_start_address_and_set_cursor_pos_boot_sector(
    start_addr: u16,
    row: u8,
    col: u8,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    let start_hi = (start_addr >> 8) as u8;
    let start_lo = (start_addr & 0x00FF) as u8;

    // Program CRTC start address regs (0x0C/0x0D).
    // mov dx, 0x3D4
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD4, 0x03]);
    i += 3;
    // mov al, 0x0C
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x0C]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // mov dx, 0x3D5
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD5, 0x03]);
    i += 3;
    // mov al, start_hi
    sector[i..i + 2].copy_from_slice(&[0xB0, start_hi]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // mov dx, 0x3D4
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD4, 0x03]);
    i += 3;
    // mov al, 0x0D
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x0D]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // mov dx, 0x3D5
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD5, 0x03]);
    i += 3;
    // mov al, start_lo
    sector[i..i + 2].copy_from_slice(&[0xB0, start_lo]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

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

fn build_set_start_address_and_set_mode_03h_boot_sector(start_addr: u16) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    let start_hi = (start_addr >> 8) as u8;
    let start_lo = (start_addr & 0x00FF) as u8;

    // Program CRTC start address regs (0x0C/0x0D).
    // mov dx, 0x3D4
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD4, 0x03]);
    i += 3;
    // mov al, 0x0C
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x0C]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // mov dx, 0x3D5
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD5, 0x03]);
    i += 3;
    // mov al, start_hi
    sector[i..i + 2].copy_from_slice(&[0xB0, start_hi]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // mov dx, 0x3D4
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD4, 0x03]);
    i += 3;
    // mov al, 0x0D
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x0D]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // mov dx, 0x3D5
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD5, 0x03]);
    i += 3;
    // mov al, start_lo
    sector[i..i + 2].copy_from_slice(&[0xB0, start_lo]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // mov ax, 0x0003 ; INT 10h AH=00h Set Video Mode (mode 03h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x03, 0x00]);
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

fn read_crtc_start_addr(m: &mut Machine) -> u16 {
    m.io_write(0x3D4, 1, 0x0C);
    let hi = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0D);
    let lo = m.io_read(0x3D5, 1) as u8;
    ((hi as u16) << 8) | (lo as u16)
}

#[test]
fn boot_int10_cursor_updates_sync_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
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
fn boot_int10_set_cursor_pos_non_active_page_does_not_move_vga_cursor() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    })
    .unwrap();

    let row = 5u8;
    let col = 10u8;
    let boot = build_int10_set_cursor_pos_on_page_boot_sector(1, row, col);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Guest updated the cursor position for page 1, but did not change the active page (default is
    // page 0). The VGA cursor overlay should remain at the active page's cursor position.
    assert_eq!(m.read_physical_u8(BDA_ACTIVE_PAGE_ADDR), 0);
    let page1_word = m.read_physical_u16(BDA_CURSOR_POS_PAGE0_ADDR + 2);
    assert_eq!(page1_word, (u16::from(row) << 8) | u16::from(col));

    let expected_pos = read_crtc_start_addr(&mut m) & 0x3FFF;
    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos & 0x3FFF, expected_pos);
}

#[test]
fn boot_int10_cursor_shape_updates_sync_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
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

#[test]
fn boot_int10_cursor_sync_includes_crtc_start_address_offset() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    })
    .unwrap();

    let start_addr = 0x0800u16;
    let row = 5u8;
    let col = 10u8;
    let boot = build_set_start_address_and_set_cursor_pos_boot_sector(start_addr, row, col);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    let got_start = read_crtc_start_addr(&mut m);
    assert_eq!(got_start, start_addr);

    let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
    let cell_index = u16::from(row)
        .saturating_mul(cols)
        .saturating_add(u16::from(col));
    let expected_pos = start_addr.wrapping_add(cell_index) & 0x3FFF;

    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos, expected_pos);
}

#[test]
fn boot_int10_set_mode_resets_crtc_start_address() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    })
    .unwrap();

    let start_addr = 0x0800u16;
    let boot = build_set_start_address_and_set_mode_03h_boot_sector(start_addr);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Mode set 03h should reinitialize text state, including resetting the CRTC start address.
    let got_start = read_crtc_start_addr(&mut m);
    assert_eq!(got_start, 0);
}

#[test]
fn boot_int10_teletype_output_advances_cursor_and_syncs_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    })
    .unwrap();

    let boot = build_int10_teletype_boot_sector(b'A', 0x00);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Cursor position should have advanced (BDA is authoritative, but we assert the CRTC regs are
    // also updated).
    let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
    let pos_word = m.read_physical_u16(BDA_CURSOR_POS_PAGE0_ADDR);
    let row = pos_word >> 8;
    let col = pos_word & 0x00FF;
    let expected_pos = row.saturating_mul(cols).saturating_add(col);

    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos, expected_pos);
}

#[test]
fn boot_int10_set_active_page_updates_crtc_start_address() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    })
    .unwrap();

    let page = 1u8;
    let boot = build_int10_set_active_page_boot_sector(page);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    assert_eq!(m.read_physical_u8(BDA_ACTIVE_PAGE_ADDR), page);
    let page_size_bytes = m.read_physical_u16(BDA_PAGE_SIZE_ADDR);
    let expected_start = u16::from(page).saturating_mul(page_size_bytes / 2) & 0x3FFF;

    let got_start = read_crtc_start_addr(&mut m) & 0x3FFF;
    assert_eq!(got_start, expected_start);

    // Cursor location should also move to the active page's start (cursor pos defaults to 0,0).
    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos, expected_start);
}

#[test]
fn boot_int10_set_active_page_uses_that_pages_cursor_pos_for_vga_cursor() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    })
    .unwrap();

    let page = 1u8;
    let row = 5u8;
    let col = 10u8;
    let boot = build_int10_set_cursor_pos_on_page_then_select_active_page_boot_sector(
        page, row, col, page,
    );
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    assert_eq!(m.read_physical_u8(BDA_ACTIVE_PAGE_ADDR), page);

    let page_size_bytes = m.read_physical_u16(BDA_PAGE_SIZE_ADDR);
    let expected_start = u16::from(page).saturating_mul(page_size_bytes / 2) & 0x3FFF;

    let got_start = read_crtc_start_addr(&mut m) & 0x3FFF;
    assert_eq!(got_start, expected_start);

    let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
    let cell_index = u16::from(row)
        .saturating_mul(cols)
        .saturating_add(u16::from(col));
    let expected_cursor = expected_start.wrapping_add(cell_index) & 0x3FFF;

    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos & 0x3FFF, expected_cursor);
}

#[test]
fn boot_int10_write_string_updates_cursor_and_syncs_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    })
    .unwrap();

    let boot = build_int10_write_string_boot_sector();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // The BIOS should have advanced the cursor by 3 cells (after writing "ABC").
    let start_addr = read_crtc_start_addr(&mut m) & 0x3FFF;
    let expected_pos = start_addr.wrapping_add(3) & 0x3FFF;

    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos & 0x3FFF, expected_pos);
}

#[test]
fn boot_int10_aerogpu_cursor_updates_sync_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this port-mirroring test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    let boot = build_int10_set_cursor_pos_boot_sector(5, 10);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Cursor shape is initialized by BIOS POST, but the HLE BIOS does not perform VGA port I/O.
    // Ensure the machine syncs BDA cursor state into the AeroGPU legacy VGA frontend's CRTC regs.
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
fn boot_int10_aerogpu_set_cursor_pos_non_active_page_does_not_move_vga_cursor() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this port-mirroring test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    let row = 5u8;
    let col = 10u8;
    let boot = build_int10_set_cursor_pos_on_page_boot_sector(1, row, col);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Guest updated the cursor position for page 1, but did not change the active page (default is
    // page 0). The VGA cursor overlay should remain at the active page's cursor position.
    assert_eq!(m.read_physical_u8(BDA_ACTIVE_PAGE_ADDR), 0);
    let page1_word = m.read_physical_u16(BDA_CURSOR_POS_PAGE0_ADDR + 2);
    assert_eq!(page1_word, (u16::from(row) << 8) | u16::from(col));

    let expected_pos = read_crtc_start_addr(&mut m) & 0x3FFF;
    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos & 0x3FFF, expected_pos);
}

#[test]
fn boot_int10_aerogpu_cursor_shape_updates_sync_to_vga_crtc() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this port-mirroring test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
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

#[test]
fn boot_int10_aerogpu_set_active_page_uses_that_pages_cursor_pos_for_crtc() {
    let mut m = Machine::new(MachineConfig {
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
    })
    .unwrap();

    let page = 1u8;
    let row = 5u8;
    let col = 10u8;
    let boot = build_int10_set_cursor_pos_on_page_then_select_active_page_boot_sector(
        page, row, col, page,
    );
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    assert_eq!(m.read_physical_u8(BDA_ACTIVE_PAGE_ADDR), page);

    let page_size_bytes = m.read_physical_u16(BDA_PAGE_SIZE_ADDR);
    let expected_start = u16::from(page).saturating_mul(page_size_bytes / 2) & 0x3FFF;

    let got_start = read_crtc_start_addr(&mut m) & 0x3FFF;
    assert_eq!(got_start, expected_start);

    let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
    let cell_index = u16::from(row)
        .saturating_mul(cols)
        .saturating_add(u16::from(col));
    let expected_cursor = expected_start.wrapping_add(cell_index) & 0x3FFF;

    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos & 0x3FFF, expected_cursor);
}

#[test]
fn boot_int10_aerogpu_cursor_sync_includes_crtc_start_address_offset() {
    let mut m = Machine::new(MachineConfig {
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
    })
    .unwrap();

    let start_addr = 0x0800u16;
    let row = 5u8;
    let col = 10u8;
    let boot = build_set_start_address_and_set_cursor_pos_boot_sector(start_addr, row, col);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    let got_start = read_crtc_start_addr(&mut m);
    assert_eq!(got_start, start_addr);

    let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
    let cell_index = u16::from(row)
        .saturating_mul(cols)
        .saturating_add(u16::from(col));
    let expected_pos = start_addr.wrapping_add(cell_index) & 0x3FFF;

    let (start, end, pos) = read_crtc_cursor_regs(&mut m);
    assert_eq!(start, 0x06);
    assert_eq!(end, 0x07);
    assert_eq!(pos, expected_pos);
}

#[test]
fn boot_int10_aerogpu_set_mode_resets_crtc_start_address() {
    let mut m = Machine::new(MachineConfig {
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
    })
    .unwrap();

    let start_addr = 0x0800u16;
    let boot = build_set_start_address_and_set_mode_03h_boot_sector(start_addr);
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Mode set 03h should reinitialize text state, including resetting the CRTC start address.
    let got_start = read_crtc_start_addr(&mut m);
    assert_eq!(got_start, 0);
}
