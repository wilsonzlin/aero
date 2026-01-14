use firmware::bios::{build_bios_rom, VGA_FONT_8X16_OFFSET, VGA_FONT_8X8_OFFSET};

#[test]
fn bios_rom_font_tables_include_cp437_box_drawing_glyphs() {
    // Choose a CP437 box-drawing character in the 0xC0..0xDF range.
    //
    // 0xC4 is "─" (box drawings light horizontal) in CP437.
    let ch: usize = 0xC4;
    let rom = build_bios_rom();

    let off_8x16 = VGA_FONT_8X16_OFFSET as usize + ch * 16;
    let glyph_8x16 = &rom[off_8x16..off_8x16 + 16];
    assert!(
        glyph_8x16.iter().any(|&b| b != 0),
        "expected non-blank 8x16 glyph for CP437 char 0x{ch:02X}, got {glyph_8x16:?}"
    );

    let off_8x8 = VGA_FONT_8X8_OFFSET as usize + ch * 8;
    let glyph_8x8 = &rom[off_8x8..off_8x8 + 8];
    assert!(
        glyph_8x8.iter().any(|&b| b != 0),
        "expected non-blank 8x8 glyph for CP437 char 0x{ch:02X}, got {glyph_8x8:?}"
    );
}
