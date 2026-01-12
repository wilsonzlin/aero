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
    let mut font = [0u8; 256 * 16];
    for ch in 0u16..=0xFF {
        let glyph8 = BASIC_FONTS.get((ch as u8) as char).unwrap_or([0u8; 8]);
        let base = usize::from(ch as u8) * 16;
        for (row, bits) in glyph8.iter().copied().enumerate() {
            font[base + row * 2] = bits;
            font[base + row * 2 + 1] = bits;
        }
    }
    font
}

fn build_font8x8() -> [u8; 256 * 8] {
    let mut font = [0u8; 256 * 8];
    for ch in 0u16..=0xFF {
        let glyph8 = BASIC_FONTS.get((ch as u8) as char).unwrap_or([0u8; 8]);
        let base = usize::from(ch as u8) * 8;
        font[base..base + 8].copy_from_slice(&glyph8);
    }
    font
}
