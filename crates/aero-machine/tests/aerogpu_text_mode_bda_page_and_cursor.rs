use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("machine did not halt within budget");
}

fn build_boot_sector_select_page1_set_cursor_and_write_space(
    attr: u8,
    cursor_start: u8,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x0501  ; AH=05 Select Active Display Page, AL=1
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x05]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ah, 0x02  ; INT 10h AH=02h Set Cursor Position
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x02]);
    i += 2;
    // mov bh, 0x01  ; page 1
    sector[i..i + 2].copy_from_slice(&[0xB7, 0x01]);
    i += 2;
    // mov dh, 0x00  ; row 0
    sector[i..i + 2].copy_from_slice(&[0xB6, 0x00]);
    i += 2;
    // mov dl, 0x00  ; col 0
    sector[i..i + 2].copy_from_slice(&[0xB2, 0x00]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ah, 0x01  ; INT 10h AH=01h Set Cursor Shape
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x01]);
    i += 2;
    // mov ch, cursor_start
    sector[i..i + 2].copy_from_slice(&[0xB5, cursor_start]);
    i += 2;
    // mov cl, 0x00  ; end scanline 0 (1 scanline cursor)
    sector[i..i + 2].copy_from_slice(&[0xB1, 0x00]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write a space cell (attr in high byte) into the first cell of page 1 so:
    // - page selection is visible via the background color, and
    // - cursor inversion is easy to detect (space glyph is all background pixels).
    //
    // Page size in BIOS mode 03h is 80*25*2 = 4000 bytes (0x0FA0), so page 1 base is B800:0FA0.
    //
    // mov ax, 0xB800
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xB8]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov di, 0x0FA0
    sector[i..i + 3].copy_from_slice(&[0xBF, 0xA0, 0x0F]);
    i += 3;
    // mov ax, imm16 (attr<<8 | ' ')
    sector[i..i + 3].copy_from_slice(&[0xB8, b' ', attr]);
    i += 3;
    // mov [es:di], ax
    sector[i..i + 3].copy_from_slice(&[0x26, 0x89, 0x05]);
    i += 3;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn aerogpu_text_mode_page_selection_and_cursor_render_from_bda() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    // White-on-blue.
    let boot = build_boot_sector_select_page1_set_cursor_and_write_space(0x1F, 0x00);
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    m.display_present();
    let (w, _h) = m.display_resolution();
    let fb = m.display_framebuffer();

    // Text mode uses fixed 80x25 cells of 9x16 pixels.
    assert_eq!(w, 80 * 9);

    // Cursor is a 1-scanline overlay at scanline 0; check:
    // - scanline 0 is foreground (white)
    // - scanline 1 remains the background (blue)
    assert_eq!(fb[0], 0xFFFF_FFFF);
    assert_eq!(fb[w as usize], 0xFFAA_0000);
}

#[test]
fn aerogpu_text_mode_cursor_disable_bit_respected() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    // White-on-blue, but cursor disabled via CH bit5.
    let boot = build_boot_sector_select_page1_set_cursor_and_write_space(0x1F, 0x20);
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    m.display_present();
    let (w, _h) = m.display_resolution();
    let fb = m.display_framebuffer();

    assert_eq!(w, 80 * 9);
    // No cursor overlay: both scanlines are background blue.
    assert_eq!(fb[0], 0xFFAA_0000);
    assert_eq!(fb[w as usize], 0xFFAA_0000);
}
