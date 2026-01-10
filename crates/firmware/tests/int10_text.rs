use firmware::{
    bda::{BiosDataArea, BDA_VIDEO_MODE_ADDR},
    bios::Bios,
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

    assert_eq!(BiosDataArea::read_video_mode(&mem), 0x03);
    assert_eq!(BiosDataArea::read_screen_cols(&mem), 80);
    assert_eq!(BiosDataArea::read_page_size(&mem), 80 * 25 * 2);
    assert_eq!(BiosDataArea::read_active_page(&mem), 0);

    // Place cursor at (row=2, col=5).
    cpu.set_ax(0x0200);
    cpu.set_bx(0x0000); // page 0
    cpu.set_dx((2u16 << 8) | 5u16);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mem), (2, 5));

    // Write 'A' with attribute 0x1F.
    cpu.set_ax(0x0E41);
    cpu.set_bx(0x001F);
    bios.handle_int10(&mut cpu, &mut mem);

    let cell_off = (2u32 * 80 + 5) * 2;
    let addr = 0xB8000u64 + cell_off as u64;
    assert_eq!(mem.read_u8(addr), b'A');
    assert_eq!(mem.read_u8(addr + 1), 0x1F);

    // Cursor advanced.
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mem), (2, 6));

    // Sanity check: BDA video mode lives at 0x0449.
    assert_eq!(BDA_VIDEO_MODE_ADDR, 0x0449);
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
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mem), (0, 0));

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
    assert_eq!(BiosDataArea::read_cursor_pos_page0(&mem), (0, 0));
}
