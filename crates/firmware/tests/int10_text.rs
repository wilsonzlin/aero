use firmware::{
    bda::{BiosDataArea, BDA_VIDEO_MODE_ADDR},
    bios::{Bios, BIOS_SEGMENT, VGA_FONT_8X16_OFFSET},
    cpu::CpuState,
    memory::{MemoryBus, VecMemory},
    rtc::{CmosRtc, DateTime},
};

#[test]
fn int10_text_mode_teletype_updates_text_buffer_and_cursor() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(BiosDataArea::read_video_mode(&mut mem), 0x03);
    assert_eq!(BiosDataArea::read_screen_cols(&mut mem), 80);
    assert_eq!(BiosDataArea::read_text_rows(&mut mem), 25);
    assert_eq!(BiosDataArea::read_page_size(&mut mem), 80 * 25 * 2);
    assert_eq!(BiosDataArea::read_video_page_offset(&mut mem), 0);
    assert_eq!(BiosDataArea::read_active_page(&mut mem), 0);
    assert_eq!(BiosDataArea::read_crtc_base(&mut mem), 0x3D4);

    // Place cursor at (row=2, col=5).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0000); // page 0
    cpu.set_dx((2u16 << 8) | 5u16);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (2, 5));

    // Write 'A' with attribute 0x1F.
    cpu.set_ax(0x0E41);
    cpu.set_bx(0x001F);
    bios.handle_int10(&mut cpu, &mut mem);

    let cell_off = (2u32 * 80 + 5) * 2;
    let addr = 0xB8000u64 + cell_off as u64;
    assert_eq!(mem.read_u8(addr), b'A');
    assert_eq!(mem.read_u8(addr + 1), 0x1F);

    // Cursor advanced.
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (2, 6));

    // Sanity check: BDA video mode lives at 0x0449.
    assert_eq!(BDA_VIDEO_MODE_ADDR, 0x0449);
}

#[test]
fn int10_teletype_backspace_wraps_to_previous_line_when_at_col0() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Cursor at (row=1, col=0).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0000);
    cpu.set_dx((1u16 << 8) | 0u16);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (1, 0));

    // AH=0Eh: backspace.
    cpu.set_ax(0x0E08);
    cpu.set_bx(0x0007);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (0, 79));
}

#[test]
fn int10_write_char_attr_repeats_without_moving_cursor() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Place cursor at (0,0).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0000);
    cpu.set_dx(0x0000);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (0, 0));

    // AH=09: write 'X' with attribute 0x1E, three times.
    cpu.set_ax(0x0958); // AH=09, AL='X'
    cpu.set_bx(0x001E); // BH=0 page, BL=attr
    cpu.set_cx(3);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(mem.read_u8(0xB8000), b'X');
    assert_eq!(mem.read_u8(0xB8001), 0x1E);
    assert_eq!(mem.read_u8(0xB8002), b'X');
    assert_eq!(mem.read_u8(0xB8003), 0x1E);
    assert_eq!(mem.read_u8(0xB8004), b'X');
    assert_eq!(mem.read_u8(0xB8005), 0x1E);

    // Cursor remains unchanged.
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (0, 0));
}

#[test]
fn int10_set_mode_03h_respects_no_clear_bit() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h (clear).
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Write a marker into the top-left cell.
    mem.write_u8(0xB8000, b'A');
    mem.write_u8(0xB8001, 0x1F);

    // Set mode 03h again with bit7 set => no clear.
    cpu.set_ax(0x0083);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(BiosDataArea::read_video_mode(&mut mem), 0x03);
    assert_eq!(mem.read_u8(0xB8000), b'A');
    assert_eq!(mem.read_u8(0xB8001), 0x1F);
}

#[test]
fn int10_text_active_page_affects_cursor_and_scroll() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Select active page 1 (AH=05h).
    cpu.set_ax(0x0501);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(BiosDataArea::read_active_page(&mut mem), 1);
    assert_eq!(
        BiosDataArea::read_video_page_offset(&mut mem),
        BiosDataArea::read_page_size(&mut mem)
    );

    // Set cursor pos for page 1 to (row=2, col=5).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0100); // BH=page 1
    cpu.set_dx((2u16 << 8) | 5u16);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(BiosDataArea::read_cursor_pos(&mut mem, 1), (2, 5));

    // Teletype output 'A' with attribute 0x1F to page 1.
    cpu.set_ax(0x0E41);
    cpu.set_bx(0x011F);
    bios.handle_int10(&mut cpu, &mut mem);

    let page_size = BiosDataArea::read_page_size(&mut mem) as u32;
    let page1_base = 0xB8000u64 + page_size as u64;
    let cell_off = (2u32 * 80 + 5) * 2;
    assert_eq!(mem.read_u8(page1_base + cell_off as u64), b'A');
    assert_eq!(mem.read_u8(page1_base + cell_off as u64 + 1), 0x1F);
    assert_eq!(BiosDataArea::read_cursor_pos(&mut mem, 1), (2, 6));

    // Ensure page 0 is unaffected at the same location.
    let page0_base = 0xB8000u64;
    assert_eq!(mem.read_u8(page0_base + cell_off as u64), b' ');
    assert_eq!(mem.read_u8(page0_base + cell_off as u64 + 1), 0x07);

    // Scroll active page (page 1) up by one line (AH=06h).
    //
    // Place markers in page 1:
    // - row 0 col 0: 'Y'
    // - row 1 col 0: 'X'
    mem.write_u8(page1_base, b'Y');
    mem.write_u8(page1_base + 1, 0x2E);
    mem.write_u8(page1_base + (80 * 2) as u64, b'X');
    mem.write_u8(page1_base + (80 * 2 + 1) as u64, 0x1E);

    cpu.set_ax(0x0601); // scroll 1 line
    cpu.set_bx(0x0700); // BH=fill attr 0x07
    cpu.set_cx(0x0000); // top row/col
    cpu.set_dx((24u16 << 8) | 79u16); // bottom row/col
    bios.handle_int10(&mut cpu, &mut mem);

    // The top-left cell should now contain the old row 1 value ('X').
    assert_eq!(mem.read_u8(page1_base), b'X');
    assert_eq!(mem.read_u8(page1_base + 1), 0x1E);
    // And page 0 should still be untouched.
    assert_eq!(mem.read_u8(page0_base), b' ');
    assert_eq!(mem.read_u8(page0_base + 1), 0x07);
}

#[test]
fn int10_read_char_attr_at_cursor_reads_text_cell() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Place cursor at (row=3, col=4).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0000);
    cpu.set_dx((3u16 << 8) | 4u16);
    bios.handle_int10(&mut cpu, &mut mem);

    // AH=09: write 'Z' with attribute 0x1E at cursor (does not move cursor).
    cpu.set_ax(0x095A);
    cpu.set_bx(0x001E);
    cpu.set_cx(1);
    bios.handle_int10(&mut cpu, &mut mem);

    // AH=08: read back character/attribute.
    cpu.set_ax(0x0800);
    cpu.set_bx(0x0000);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(cpu.al(), b'Z');
    assert_eq!(cpu.ah(), 0x1E);
}

#[test]
fn int10_scroll_down_moves_screen_contents_down() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Place markers in the top-left column.
    // row 0 col 0: 'A'
    // row 1 col 0: 'B'
    mem.write_u8(0xB8000, b'A');
    mem.write_u8(0xB8001, 0x1F);
    mem.write_u8(0xB8000 + (80 * 2) as u64, b'B');
    mem.write_u8(0xB8000 + (80 * 2 + 1) as u64, 0x2E);

    // AH=07h: scroll down by 1 line.
    cpu.set_ax(0x0701);
    cpu.set_bx(0x0700); // BH=fill attr 0x07
    cpu.set_cx(0x0000); // top row/col
    cpu.set_dx((24u16 << 8) | 79u16); // bottom row/col
    bios.handle_int10(&mut cpu, &mut mem);

    // The top-left cell should now be blank, and row 1 should contain the old row 0 value.
    assert_eq!(mem.read_u8(0xB8000), b' ');
    assert_eq!(mem.read_u8(0xB8001), 0x07);
    assert_eq!(mem.read_u8(0xB8000 + (80 * 2) as u64), b'A');
    assert_eq!(mem.read_u8(0xB8000 + (80 * 2 + 1) as u64), 0x1F);
    assert_eq!(mem.read_u8(0xB8000 + (2 * 80 * 2) as u64), b'B');
    assert_eq!(mem.read_u8(0xB8000 + (2 * 80 * 2 + 1) as u64), 0x2E);
}

#[test]
fn int10_write_string_writes_chars_and_updates_cursor() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Write "HI" starting at (row=0,col=0) with attribute 0x1E.
    mem.write_u8(0x2000, b'H');
    mem.write_u8(0x2001, b'I');

    cpu.set_ax(0x1301); // AH=13h write string, AL=01h (update cursor, no attrs in string)
    cpu.set_bx(0x001E); // BH=page0, BL=attr
    cpu.set_cx(2);
    cpu.set_dx(0x0000); // row 0, col 0
    cpu.set_es(0x0000);
    cpu.set_bp(0x2000);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(mem.read_u8(0xB8000), b'H');
    assert_eq!(mem.read_u8(0xB8001), 0x1E);
    assert_eq!(mem.read_u8(0xB8002), b'I');
    assert_eq!(mem.read_u8(0xB8003), 0x1E);

    // Cursor advanced to col 2.
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (0, 2));
}

#[test]
fn int10_write_string_with_inline_attributes_respects_each_cell_attr() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // String with inline attributes: ('A',0x1F), ('B',0x2E).
    mem.write_u8(0x2100, b'A');
    mem.write_u8(0x2101, 0x1F);
    mem.write_u8(0x2102, b'B');
    mem.write_u8(0x2103, 0x2E);

    cpu.set_ax(0x1303); // AL=03h (update cursor + attrs in string)
    cpu.set_bx(0x0000); // BH=page0
    cpu.set_cx(2);
    cpu.set_dx(0x0000);
    cpu.set_es(0x0000);
    cpu.set_bp(0x2100);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(mem.read_u8(0xB8000), b'A');
    assert_eq!(mem.read_u8(0xB8001), 0x1F);
    assert_eq!(mem.read_u8(0xB8002), b'B');
    assert_eq!(mem.read_u8(0xB8003), 0x2E);
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (0, 2));
}

#[test]
fn int10_write_string_without_update_cursor_leaves_cursor_unchanged() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Move cursor to (row=2,col=3).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0000);
    cpu.set_dx((2u16 << 8) | 3u16);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (2, 3));

    mem.write_u8(0x2200, b'X');
    cpu.set_ax(0x1300); // AL=00h (do not update cursor)
    cpu.set_bx(0x001E);
    cpu.set_cx(1);
    cpu.set_dx(0x0000); // write at 0,0
    cpu.set_es(0x0000);
    cpu.set_bp(0x2200);
    bios.handle_int10(&mut cpu, &mut mem);

    // Cursor unchanged.
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (2, 3));
    // But the character was written at the requested location.
    assert_eq!(mem.read_u8(0xB8000), b'X');
}

#[test]
fn int10_write_char_only_preserves_existing_attribute_and_cursor() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 03h.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    // Put distinct attributes in two adjacent cells.
    mem.write_u8(0xB8000, b'A');
    mem.write_u8(0xB8001, 0x2E);
    mem.write_u8(0xB8002, b'B');
    mem.write_u8(0xB8003, 0x1F);

    // Cursor at (0,0).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0000);
    cpu.set_dx(0x0000);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (0, 0));

    // AH=0Ah: write 'X' twice without modifying attributes.
    cpu.set_ax(0x0A58); // AH=0Ah, AL='X'
    cpu.set_bx(0x0000); // page 0
    cpu.set_cx(2);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(mem.read_u8(0xB8000), b'X');
    assert_eq!(mem.read_u8(0xB8001), 0x2E);
    assert_eq!(mem.read_u8(0xB8002), b'X');
    assert_eq!(mem.read_u8(0xB8003), 0x1F);
    // Cursor remains unchanged.
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mut mem), (0, 0));
}

#[test]
fn int10_get_font_info_returns_8x16_font_pointer() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    cpu.rcx = 0x1234_5678_0000_0000;
    cpu.rdx = 0xFEDC_BA98_0000_1200;

    // Initialize text-mode BDA fields so rows-1 is well-defined.
    cpu.set_ax(0x0003);
    bios.handle_int10(&mut cpu, &mut mem);

    cpu.set_ax(0x1130); // AH=11h, AL=30h
    cpu.set_bh(0x06); // request 8x16 font (common convention)

    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(cpu.es(), BIOS_SEGMENT);
    assert_eq!(cpu.bp(), VGA_FONT_8X16_OFFSET);
    assert_eq!(cpu.cx(), 16);
    assert_eq!(cpu.dl(), 24);

    // Preserve upper register bits.
    assert_eq!(cpu.rcx & !0xFFFF, 0x1234_5678_0000_0000);
    assert_eq!(cpu.rdx & !0xFF, 0xFEDC_BA98_0000_1200);
}
