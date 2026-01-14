use super::{
    BIOS_SIZE, DEFAULT_INT_STUB_OFFSET, DISKETTE_PARAM_TABLE_OFFSET, FIXED_DISK_PARAM_TABLE_OFFSET,
    INT10_STUB_OFFSET, INT13_STUB_OFFSET, INT15_STUB_OFFSET, INT16_STUB_OFFSET, INT1A_STUB_OFFSET,
    VGA_FONT_8X16_OFFSET, VGA_FONT_8X8_OFFSET, VIDEO_PARAM_TABLE_OFFSET,
};

use font8x8::{UnicodeFonts, BASIC_FONTS};

/// Build the 64KiB BIOS ROM image.
///
/// We only embed tiny interrupt stubs used for HLE dispatch:
/// `HLT; IRET`.
pub fn build_bios_rom() -> Vec<u8> {
    let mut rom = vec![0xFFu8; BIOS_SIZE];

    // Install a conventional x86 reset vector at F000:FFF0.
    //
    // Aero performs POST in host code, but guests and tooling may still expect
    // the reset vector to contain a FAR JMP instruction.
    //
    // Encoding: JMP FAR ptr16:16 => EA iw (offset) iw (segment)
    // Target: F000:E000.
    let reset_off = 0xFFF0usize;
    rom[reset_off] = 0xEA;
    rom[reset_off + 1] = 0x00; // offset low
    rom[reset_off + 2] = 0xE0; // offset high (0xE000)
    rom[reset_off + 3] = 0x00; // segment low
    rom[reset_off + 4] = 0xF0; // segment high (0xF000)

    // Safe fallback at F000:E000: `cli; hlt; jmp $-2`.
    //
    // In a full-system integration this address is never reached because POST
    // is performed in host code, but it provides deterministic behavior if it is.
    let stub_off = 0xE000usize;
    rom[stub_off] = 0xFA;
    rom[stub_off + 1] = 0xF4;
    rom[stub_off + 2] = 0xEB;
    rom[stub_off + 3] = 0xFE;

    let stub = [0xF4u8, 0xCFu8]; // HLT; IRET
    write_stub(&mut rom, DEFAULT_INT_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT10_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT13_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT15_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT16_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT1A_STUB_OFFSET, &stub);

    // Diskette Parameter Table (IVT vector 0x1E).
    //
    // This is an 11-byte table traditionally used by DOS-era software to probe or patch floppy
    // timing/geometry parameters. Our floppy implementation is fully emulated in software, but
    // providing a reasonable table improves compatibility with guests that expect it to exist.
    //
    // Values below match common 1.44MiB defaults:
    // - 512 bytes/sector, 18 sectors/track.
    let diskette_param_table: [u8; 11] = [
        0xAF, 0x02, 0x25, 0x02, 0x12, 0x1B, 0xFF, 0x6C, 0xF6, 0x0F, 0x08,
    ];
    write_stub(&mut rom, DISKETTE_PARAM_TABLE_OFFSET, &diskette_param_table);

    // Fixed Disk Parameter Table (IVT vectors 0x41/0x46).
    //
    // Older software reads these vectors to obtain CHS geometry for BIOS disk services.
    // We provide a table that matches our "fixed disk" INT 13h geometry (1024/16/63).
    //
    // Table format is 16 bytes (IBM PC/AT):
    // - word: cylinders
    // - byte: heads
    // - word: write precomp cylinder
    // - byte: control
    // - word: landing zone
    // - byte: sectors/track
    // - remaining bytes reserved (0)
    let fixed_disk_param_table: [u8; 16] = [
        0x00, 0x04, // cylinders = 1024
        0x10, // heads = 16
        0x00, 0x00, // write precomp = 0
        0x00, // control
        0x00, 0x00, // landing zone = 0
        0x3F, // sectors/track = 63
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
    ];
    write_stub(
        &mut rom,
        FIXED_DISK_PARAM_TABLE_OFFSET,
        &fixed_disk_param_table,
    );

    // Video parameter table (IVT vector 0x1D).
    //
    // Many DOS-era programs read this pointer directly for timing/CRTC defaults. We provide a
    // small, VGA-compatible table for text mode.
    //
    // This is not a complete hardware model; the INT 10h VGA implementation is the source of truth.
    let video_param_table: [u8; 16] = [
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00, 0x00, 0x00,
        0x00,
    ];
    write_stub(&mut rom, VIDEO_PARAM_TABLE_OFFSET, &video_param_table);

    // VGA font table (INT 10h AH=11h AL=30h).
    //
    // DOS-era software commonly retrieves the ROM font and expects the CP437 "line graphics" range
    // (0xB3..=0xDF) to be populated with box drawing/block glyphs. Our VGA text renderer has a
    // CP437-compatible built-in font; the BIOS ROM needs to expose similar glyphs via INT 10h so
    // guests that query the font table can render boot-time UIs without blank characters.
    let font_8x16 = build_font8x16_cp437();
    write_stub(&mut rom, VGA_FONT_8X16_OFFSET, &font_8x16);

    // IVT vector 0x1F also historically points at an 8x8 graphics character table.
    let font_8x8 = build_font8x8_cp437();
    write_stub(&mut rom, VGA_FONT_8X8_OFFSET, &font_8x8);

    // Optional ROM signature (harmless and convenient for identification).
    rom[BIOS_SIZE - 2] = 0x55;
    rom[BIOS_SIZE - 1] = 0xAA;

    rom
}

fn write_stub(rom: &mut [u8], offset: u16, stub: &[u8]) {
    let off = offset as usize;
    let end = off + stub.len();
    rom[off..end].copy_from_slice(stub);
}

fn build_font8x16_cp437() -> [u8; 256 * 16] {
    // Generate the 8x16 font by vertically scaling the 8x8 table (duplicate each scanline).
    //
    // VGA BIOS INT 10h AH=11h AL=30h returns a pointer to this 8x16 table, and DOS-era software
    // often indexes it using CP437 codepoints (including the box-drawing range).
    let font8 = build_font8x8_cp437();
    let mut font16 = [0u8; 256 * 16];

    for ch in 0u16..=0xFF {
        let base8 = usize::from(ch as u8) * 8;
        let base16 = usize::from(ch as u8) * 16;
        for row in 0usize..8 {
            let bits = font8[base8 + row];
            font16[base16 + row * 2] = bits;
            font16[base16 + row * 2 + 1] = bits;
        }
    }

    font16
}

fn build_font8x8_cp437() -> [u8; 256 * 8] {
    let mut font = [0u8; 256 * 8];

    // Preserve the original 0x00..=0x7F glyphs.
    for ch in 0u16..=0x7F {
        let glyph8 = BASIC_FONTS.get((ch as u8) as char).unwrap_or([0u8; 8]);
        let base = usize::from(ch as u8) * 8;
        font[base..base + 8].copy_from_slice(&glyph8);
    }

    set_glyph8(&mut font, 0xB0, glyph_shade_light()); // ░
    set_glyph8(&mut font, 0xB1, glyph_shade_medium()); // ▒
    set_glyph8(&mut font, 0xB2, glyph_shade_dark()); // ▓

    // Single-line box drawing.
    set_glyph8(&mut font, 0xB3, glyph_box_single(true, true, false, false)); // │
    set_glyph8(&mut font, 0xB4, glyph_box_single(true, true, true, false)); // ┤
    set_glyph8(&mut font, 0xBF, glyph_box_single(false, true, true, false)); // ┐
    set_glyph8(&mut font, 0xC0, glyph_box_single(true, false, false, true)); // └
    set_glyph8(&mut font, 0xC1, glyph_box_single(true, false, true, true)); // ┴
    set_glyph8(&mut font, 0xC2, glyph_box_single(false, true, true, true)); // ┬
    set_glyph8(&mut font, 0xC3, glyph_box_single(true, true, false, true)); // ├
    set_glyph8(&mut font, 0xC4, glyph_box_single(false, false, true, true)); // ─
    set_glyph8(&mut font, 0xC5, glyph_box_single(true, true, true, true)); // ┼
    set_glyph8(&mut font, 0xD9, glyph_box_single(true, false, true, false)); // ┘
    set_glyph8(&mut font, 0xDA, glyph_box_single(false, true, false, true)); // ┌

    // Double-line/mixed box drawing (approximated).
    set_glyph8(&mut font, 0xB5, glyph_box_single(true, true, true, false)); // ╡
    set_glyph8(&mut font, 0xB6, glyph_box_single(true, true, true, false)); // ╢
    set_glyph8(&mut font, 0xB7, glyph_box_single(false, true, true, false)); // ╖
    set_glyph8(&mut font, 0xB8, glyph_box_single(false, true, true, false)); // ╕
    set_glyph8(&mut font, 0xB9, glyph_box_single(true, true, true, false)); // ╣
    set_glyph8(&mut font, 0xBA, glyph_box_double_vertical()); // ║
    set_glyph8(&mut font, 0xBB, glyph_box_single(false, true, true, false)); // ╗
    set_glyph8(&mut font, 0xBC, glyph_box_single(true, false, true, false)); // ╝
    set_glyph8(&mut font, 0xBD, glyph_box_single(true, false, true, false)); // ╜
    set_glyph8(&mut font, 0xBE, glyph_box_single(true, false, true, false)); // ╛

    set_glyph8(&mut font, 0xC6, glyph_box_single(true, true, false, true)); // ╞
    set_glyph8(&mut font, 0xC7, glyph_box_single(true, true, false, true)); // ╟
    set_glyph8(&mut font, 0xC8, glyph_box_single(true, false, false, true)); // ╚
    set_glyph8(&mut font, 0xC9, glyph_box_single(false, true, false, true)); // ╔
    set_glyph8(&mut font, 0xCA, glyph_box_single(true, false, true, true)); // ╩
    set_glyph8(&mut font, 0xCB, glyph_box_single(false, true, true, true)); // ╦
    set_glyph8(&mut font, 0xCC, glyph_box_single(true, true, false, true)); // ╠
    set_glyph8(&mut font, 0xCD, glyph_box_single(false, false, true, true)); // ═
    set_glyph8(&mut font, 0xCE, glyph_box_single(true, true, true, true)); // ╬
    set_glyph8(&mut font, 0xCF, glyph_box_single(true, false, true, true)); // ╧
    set_glyph8(&mut font, 0xD0, glyph_box_single(true, false, true, true)); // ╨
    set_glyph8(&mut font, 0xD1, glyph_box_single(false, true, true, true)); // ╤
    set_glyph8(&mut font, 0xD2, glyph_box_single(false, true, true, true)); // ╥
    set_glyph8(&mut font, 0xD3, glyph_box_single(true, false, false, true)); // ╙
    set_glyph8(&mut font, 0xD4, glyph_box_single(true, false, false, true)); // ╘
    set_glyph8(&mut font, 0xD5, glyph_box_single(false, true, false, true)); // ╒
    set_glyph8(&mut font, 0xD6, glyph_box_single(false, true, false, true)); // ╓
    set_glyph8(&mut font, 0xD7, glyph_box_single(true, true, true, true)); // ╫
    set_glyph8(&mut font, 0xD8, glyph_box_single(true, true, true, true)); // ╪

    // Block elements.
    set_glyph8(&mut font, 0xDB, [0xFF; 8]); // █
    set_glyph8(&mut font, 0xDC, glyph_block_lower_half()); // ▄
    set_glyph8(&mut font, 0xDD, glyph_block_left_half()); // ▌
    set_glyph8(&mut font, 0xDE, glyph_block_right_half()); // ▐
    set_glyph8(&mut font, 0xDF, glyph_block_upper_half()); // ▀

    font
}

fn set_glyph8(font: &mut [u8; 256 * 8], ch: u8, glyph: [u8; 8]) {
    let base = usize::from(ch) * 8;
    font[base..base + 8].copy_from_slice(&glyph);
}

fn glyph_box_single(up: bool, down: bool, left: bool, right: bool) -> [u8; 8] {
    let mut out = [0u8; 8];
    let h_row = 3usize;
    let v_col = 3usize;
    let v_bit = 1u8 << (7 - v_col);
    let left_mask = 0xFFu8 << (7 - v_col);
    let right_mask = 0xFFu8 >> v_col;

    for (y, out_row) in out.iter_mut().enumerate() {
        let mut row = 0u8;
        if (up && y <= h_row) || (down && y >= h_row) {
            row |= v_bit;
        }
        if y == h_row {
            if left {
                row |= left_mask;
            }
            if right {
                row |= right_mask;
            }
        }
        *out_row = row;
    }

    out
}

fn glyph_box_double_vertical() -> [u8; 8] {
    let mut out = [0u8; 8];
    let v_bits = 0x18u8; // bits 4 and 3
    out.fill(v_bits);
    out
}

fn glyph_block_upper_half() -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].fill(0xFF);
    out
}

fn glyph_block_lower_half() -> [u8; 8] {
    let mut out = [0u8; 8];
    out[4..].fill(0xFF);
    out
}

fn glyph_block_left_half() -> [u8; 8] {
    let mut out = [0u8; 8];
    out.fill(0xF0); // left 4 pixels
    out
}

fn glyph_block_right_half() -> [u8; 8] {
    let mut out = [0u8; 8];
    out.fill(0x0F); // right 4 pixels
    out
}

fn glyph_shade_light() -> [u8; 8] {
    let mut out = [0u8; 8];
    for (y, row) in out.iter_mut().enumerate() {
        *row = if (y & 1) == 0 { 0x00 } else { 0x55 };
    }
    out
}

fn glyph_shade_medium() -> [u8; 8] {
    let mut out = [0u8; 8];
    for (y, row) in out.iter_mut().enumerate() {
        *row = if (y & 1) == 0 { 0x55 } else { 0xAA };
    }
    out
}

fn glyph_shade_dark() -> [u8; 8] {
    let mut out = [0u8; 8];
    for (y, row) in out.iter_mut().enumerate() {
        *row = if (y & 1) == 0 { 0xFF } else { 0xAA };
    }
    out
}
