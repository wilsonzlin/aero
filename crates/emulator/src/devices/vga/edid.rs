pub const EDID_BLOCK_SIZE: usize = 128;

pub fn read_edid(block: u16) -> Option<[u8; EDID_BLOCK_SIZE]> {
    match block {
        0 => Some(generate_base_edid()),
        _ => None,
    }
}

fn generate_base_edid() -> [u8; EDID_BLOCK_SIZE] {
    let mut edid = [0u8; EDID_BLOCK_SIZE];

    // Header
    edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);

    // Manufacturer: "AER"
    edid[8..10].copy_from_slice(&0x04B2u16.to_be_bytes());
    // Product code (arbitrary)
    edid[10..12].copy_from_slice(&0x0001u16.to_le_bytes());
    // Serial number (unused)
    edid[12..16].copy_from_slice(&0u32.to_le_bytes());
    // Week/year of manufacture
    edid[16] = 1;
    edid[17] = 34; // 1990 + 34 = 2024
                   // EDID version/revision
    edid[18] = 1;
    edid[19] = 4;
    // Video input: digital, interface unspecified
    edid[20] = 0x80;
    // Screen size in cm
    edid[21] = 34;
    edid[22] = 27;
    // Gamma: 2.20
    edid[23] = 120;
    // Features: sRGB + preferred timing mode
    edid[24] = 0x06;

    // Chromaticity coordinates (sRGB-ish).
    edid[25] = 0xEE;
    edid[26] = 0x91;
    edid[27] = 0xA3;
    edid[28] = 0x54;
    edid[29] = 0x4C;
    edid[30] = 0x99;
    edid[31] = 0x26;
    edid[32] = 0x0F;
    edid[33] = 0x50;
    edid[34] = 0x54;

    // Established timings: 640x480@60, 800x600@60, 1024x768@60.
    edid[35] = 0x21;
    edid[36] = 0x08;
    edid[37] = 0x00;

    // Standard timings.
    edid[38..54].copy_from_slice(&[
        0x61, 0x40, // 1024x768@60
        0x45, 0x40, // 800x600@60
        0x31, 0x40, // 640x480@60
        0x01, 0x01, // unused
        0x01, 0x01, // unused
        0x01, 0x01, // unused
        0x01, 0x01, // unused
        0x01, 0x01, // unused
    ]);

    // Detailed timing descriptor #1: 1024x768@60 (VESA DMT).
    edid[54..72].copy_from_slice(&[
        0x64, 0x19, // pixel clock: 65.00 MHz
        0x00, 0x40, 0x41, // hactive=1024, hblank=320
        0x00, 0x26, 0x30, // vactive=768, vblank=38
        0x18, 0x88, // hsync offset=24, hsync pulse=136
        0x36, 0x00, // vsync offset=3, vsync pulse=6
        0x54, 0x0E, 0x11, // image size: 340mm x 270mm
        0x00, 0x00, // borders
        0x18, // flags: digital separate sync, -hsync, -vsync
    ]);

    // Detailed descriptor #2: monitor name.
    edid[72..90].copy_from_slice(&[
        0x00, 0x00, 0x00, 0xFC, 0x00, b'A', b'E', b'R', b'O', b' ', b'V', b'G', b'A', 0x0A, 0x20,
        0x20, 0x20, 0x20,
    ]);

    // Detailed descriptor #3: range limits.
    edid[90..108].copy_from_slice(&[
        0x00, 0x00, 0x00, 0xFD, 0x00, 50, 75, 30, 80, 8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ]);

    // Detailed descriptor #4: unused.
    edid[108..126].copy_from_slice(&[
        0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ]);

    // Extension block count.
    edid[126] = 0;
    edid[127] = checksum_byte(&edid);

    edid
}

fn checksum_byte(edid: &[u8; EDID_BLOCK_SIZE]) -> u8 {
    let sum = edid[..EDID_BLOCK_SIZE - 1]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    (0u8).wrapping_sub(sum)
}
