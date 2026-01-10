#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };
}

pub fn rgb_to_rgba_u32(rgb: Rgb) -> u32 {
    // RGBA in little-endian byte order is convenient for Canvas ImageData.
    (rgb.r as u32) | ((rgb.g as u32) << 8) | ((rgb.b as u32) << 16) | (0xFFu32 << 24)
}

pub fn vga_6bit_to_8bit(v: u8) -> u8 {
    // Expand 6-bit DAC component to 8-bit.
    let v = v & 0x3F;
    (v << 2) | (v >> 4)
}

pub fn vga_8bit_to_6bit(v: u8) -> u8 {
    v >> 2
}

pub fn default_vga_palette() -> [Rgb; 256] {
    let mut pal = [Rgb::BLACK; 256];

    // Standard EGA 16 colors.
    let ega = [
        Rgb {
            r: 0x00,
            g: 0x00,
            b: 0x00,
        }, // 0 black
        Rgb {
            r: 0x00,
            g: 0x00,
            b: 0xAA,
        }, // 1 blue
        Rgb {
            r: 0x00,
            g: 0xAA,
            b: 0x00,
        }, // 2 green
        Rgb {
            r: 0x00,
            g: 0xAA,
            b: 0xAA,
        }, // 3 cyan
        Rgb {
            r: 0xAA,
            g: 0x00,
            b: 0x00,
        }, // 4 red
        Rgb {
            r: 0xAA,
            g: 0x00,
            b: 0xAA,
        }, // 5 magenta
        Rgb {
            r: 0xAA,
            g: 0x55,
            b: 0x00,
        }, // 6 brown
        Rgb {
            r: 0xAA,
            g: 0xAA,
            b: 0xAA,
        }, // 7 light grey
        Rgb {
            r: 0x55,
            g: 0x55,
            b: 0x55,
        }, // 8 dark grey
        Rgb {
            r: 0x55,
            g: 0x55,
            b: 0xFF,
        }, // 9 bright blue
        Rgb {
            r: 0x55,
            g: 0xFF,
            b: 0x55,
        }, // 10 bright green
        Rgb {
            r: 0x55,
            g: 0xFF,
            b: 0xFF,
        }, // 11 bright cyan
        Rgb {
            r: 0xFF,
            g: 0x55,
            b: 0x55,
        }, // 12 bright red
        Rgb {
            r: 0xFF,
            g: 0x55,
            b: 0xFF,
        }, // 13 bright magenta
        Rgb {
            r: 0xFF,
            g: 0xFF,
            b: 0x55,
        }, // 14 yellow
        Rgb {
            r: 0xFF,
            g: 0xFF,
            b: 0xFF,
        }, // 15 white
    ];
    pal[..16].copy_from_slice(&ega);

    // 6x6x6 color cube (indices 16..231), similar to the classic VGA palette.
    let mut idx = 16usize;
    for r in 0..6u8 {
        for g in 0..6u8 {
            for b in 0..6u8 {
                let scale = |v: u8| -> u8 { ((v as u16 * 255) / 5) as u8 };
                pal[idx] = Rgb {
                    r: scale(r),
                    g: scale(g),
                    b: scale(b),
                };
                idx += 1;
            }
        }
    }

    // Grayscale ramp (232..255).
    for i in 0..24u8 {
        let v = ((i as u16 * 255) / 23) as u8;
        pal[232 + i as usize] = Rgb { r: v, g: v, b: v };
    }

    pal
}
