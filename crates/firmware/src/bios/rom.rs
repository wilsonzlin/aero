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
    // We provide a simple 8x16 bitmap font derived from `font8x8`'s BASIC font table by duplicating
    // each 8x8 scanline twice. This is not a full CP437 font, but it is sufficient for ASCII text
    // consumers that retrieve the font via BIOS calls.
    let font_8x16 = build_font8x16();
    write_stub(&mut rom, VGA_FONT_8X16_OFFSET, &font_8x16);

    // IVT vector 0x1F also historically points at an 8x8 graphics character table.
    let font_8x8 = build_font8x8();
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

fn build_font8x16() -> [u8; 256 * 16] {
    // Generate the 8x16 font by vertically scaling the 8x8 table (duplicate each scanline).
    //
    // VGA BIOS INT 10h AH=11h AL=30h returns a pointer to this 8x16 table, and DOS-era software
    // often indexes it using CP437 codepoints (including the box-drawing range).
    let font8 = build_font8x8();
    let mut font16 = [0u8; 256 * 16];
    for ch in 0usize..=0xFF {
        let src = ch * 8;
        let dst = ch * 16;
        for row in 0..8 {
            let bits = font8[src + row];
            font16[dst + row * 2] = bits;
            font16[dst + row * 2 + 1] = bits;
        }
    }
    font16
}

fn build_font8x8() -> [u8; 256 * 8] {
    let mut font = [0u8; 256 * 8];
    for ch in 0u8..=0xFF {
        let mut glyph8 = BASIC_FONTS.get(ch as char).unwrap_or([0u8; 8]);

        // DOS-era consumers retrieve the VGA font table via BIOS calls and then index it using
        // CP437 bytes. The `font8x8` table is indexed by Unicode scalar values, so we override a
        // small CP437 subset (notably the box drawing range) to improve compatibility.
        if let Some(override_glyph) = cp437_override_glyph(ch) {
            glyph8 = override_glyph;
        }

        let base = usize::from(ch) * 8;
        font[base..base + 8].copy_from_slice(&glyph8);
    }
    font
}

fn cp437_override_glyph(ch: u8) -> Option<[u8; 8]> {
    // Minimal CP437 box-drawing / block-element overrides.
    //
    // The glyph art is intentionally simple: single-line and double-line box drawing characters are
    // rendered using the same centered strokes. The goal is to avoid blank glyphs for common DOS UI
    // characters rather than perfectly reproduce VGA ROM font aesthetics.
    match ch {
        // Light box drawing.
        0xB3 => Some(box_draw_glyph(true, true, false, false)),  // │
        0xB4 => Some(box_draw_glyph(true, true, true, false)),   // ┤
        0xBF => Some(box_draw_glyph(false, true, true, false)),  // ┐
        0xC0 => Some(box_draw_glyph(true, false, false, true)),  // └
        0xC1 => Some(box_draw_glyph(true, false, true, true)),   // ┴
        0xC2 => Some(box_draw_glyph(false, true, true, true)),   // ┬
        0xC3 => Some(box_draw_glyph(true, true, false, true)),   // ├
        0xC4 => Some(box_draw_glyph(false, false, true, true)),  // ─
        0xC5 => Some(box_draw_glyph(true, true, true, true)),    // ┼

        // Double-line and mixed box drawing. Rendered with the same simple strokes.
        0xC6 => Some(box_draw_glyph(true, true, false, true)),   // ╞
        0xC7 => Some(box_draw_glyph(true, true, false, true)),   // ╟
        0xC8 => Some(box_draw_glyph(true, false, false, true)),  // ╚
        0xC9 => Some(box_draw_glyph(false, true, false, true)),  // ╔
        0xCA => Some(box_draw_glyph(true, false, true, true)),   // ╩
        0xCB => Some(box_draw_glyph(false, true, true, true)),   // ╦
        0xCC => Some(box_draw_glyph(true, true, false, true)),   // ╠
        0xCD => Some(box_draw_glyph(false, false, true, true)),  // ═
        0xCE => Some(box_draw_glyph(true, true, true, true)),    // ╬
        0xCF => Some(box_draw_glyph(true, false, true, true)),   // ╧
        0xD0 => Some(box_draw_glyph(true, false, true, true)),   // ╨
        0xD1 => Some(box_draw_glyph(false, true, true, true)),   // ╤
        0xD2 => Some(box_draw_glyph(false, true, true, true)),   // ╥
        0xD3 => Some(box_draw_glyph(true, false, false, true)),  // ╙
        0xD4 => Some(box_draw_glyph(true, false, false, true)),  // ╘
        0xD5 => Some(box_draw_glyph(false, true, false, true)),  // ╒
        0xD6 => Some(box_draw_glyph(false, true, false, true)),  // ╓
        0xD7 => Some(box_draw_glyph(true, true, true, true)),    // ╫
        0xD8 => Some(box_draw_glyph(true, true, true, true)),    // ╪
        0xD9 => Some(box_draw_glyph(true, false, true, false)),  // ┘
        0xDA => Some(box_draw_glyph(false, true, false, true)),  // ┌

        // Block elements.
        0xDB => Some([0xFF; 8]), // █
        0xDC => Some([0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF]), // ▄
        0xDD => Some([0xF0; 8]), // ▌
        0xDE => Some([0x0F; 8]), // ▐
        0xDF => Some([0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00]), // ▀
        _ => None,
    }
}

fn box_draw_glyph(up: bool, down: bool, left: bool, right: bool) -> [u8; 8] {
    // 8x8 glyph coordinates:
    // - Rows are top-to-bottom.
    // - Bits are MSB->LSB (left-to-right).
    //
    // We draw a 2-pixel-wide centered vertical stroke and a 1-pixel-high horizontal stroke,
    // positioned so that the derived 8x16 font (each row duplicated) yields a 2-pixel-high
    // horizontal line.
    const V_BITS: u8 = 0x18; // columns 3 and 4
    const H_ROW: usize = 3;

    const H_LEFT: u8 = 0xF8; // columns 0..4
    const H_RIGHT: u8 = 0x1F; // columns 3..7
    const H_FULL: u8 = 0xFF;

    let mut glyph = [0u8; 8];

    if up || down {
        let start = if up { 0 } else { H_ROW };
        let end = if down { 7 } else { H_ROW };
        for row in start..=end {
            glyph[row] |= V_BITS;
        }
    }

    if left || right {
        let bits = match (left, right) {
            (true, true) => H_FULL,
            (true, false) => H_LEFT,
            (false, true) => H_RIGHT,
            (false, false) => 0,
        };
        glyph[H_ROW] |= bits;
    }

    glyph
}
