use crate::devices::vga::ports::VgaDevice;

pub const TEXT_MODE_COLUMNS: usize = 80;
pub const TEXT_MODE_ROWS: usize = 25;

// VGA text modes use a 9x16 cell on the standard VGA timing (720x400).
pub const TEXT_MODE_CHAR_WIDTH: usize = 9;
pub const TEXT_MODE_CHAR_HEIGHT: usize = 16;

pub const TEXT_MODE_WIDTH: usize = TEXT_MODE_COLUMNS * TEXT_MODE_CHAR_WIDTH;
pub const TEXT_MODE_HEIGHT: usize = TEXT_MODE_ROWS * TEXT_MODE_CHAR_HEIGHT;
pub const TEXT_MODE_FRAMEBUFFER_SIZE: usize = TEXT_MODE_WIDTH * TEXT_MODE_HEIGHT;

const PLANE_SIZE: usize = 0x10000;

// Standard 16-colour VGA palette in 8-bit RGB.
const TEXT_PALETTE_RGBA: [u32; 16] = [
    pack_rgba(0x00, 0x00, 0x00), // 0: black
    pack_rgba(0x00, 0x00, 0xAA), // 1: blue
    pack_rgba(0x00, 0xAA, 0x00), // 2: green
    pack_rgba(0x00, 0xAA, 0xAA), // 3: cyan
    pack_rgba(0xAA, 0x00, 0x00), // 4: red
    pack_rgba(0xAA, 0x00, 0xAA), // 5: magenta
    pack_rgba(0xAA, 0x55, 0x00), // 6: brown
    pack_rgba(0xAA, 0xAA, 0xAA), // 7: light gray
    pack_rgba(0x55, 0x55, 0x55), // 8: dark gray
    pack_rgba(0x55, 0x55, 0xFF), // 9: light blue
    pack_rgba(0x55, 0xFF, 0x55), // A: light green
    pack_rgba(0x55, 0xFF, 0xFF), // B: light cyan
    pack_rgba(0xFF, 0x55, 0x55), // C: light red
    pack_rgba(0xFF, 0x55, 0xFF), // D: light magenta
    pack_rgba(0xFF, 0xFF, 0x55), // E: yellow
    pack_rgba(0xFF, 0xFF, 0xFF), // F: white
];

#[inline]
const fn pack_rgba(r: u8, g: u8, b: u8) -> u32 {
    u32::from_le_bytes([r, g, b, 0xFF])
}

#[derive(Debug)]
pub struct TextModeRenderer {
    framebuffer: Vec<u32>,
}

impl TextModeRenderer {
    pub fn new() -> Self {
        Self {
            framebuffer: vec![TEXT_PALETTE_RGBA[0]; TEXT_MODE_FRAMEBUFFER_SIZE],
        }
    }

    pub fn render<'a>(&'a mut self, regs: &VgaDevice) -> &'a [u32] {
        let vram = regs.vram();

        // Text mode uses odd/even addressing: plane 0 = character bytes, plane 1 = attribute.
        // For now we draw only the background colour (high nibble) for each cell.
        let attr_plane = &vram[PLANE_SIZE..PLANE_SIZE + 0x4000];

        for row in 0..TEXT_MODE_ROWS {
            for col in 0..TEXT_MODE_COLUMNS {
                let cell = row * TEXT_MODE_COLUMNS + col;
                let attr = attr_plane[cell];
                let bg = usize::from((attr >> 4) & 0x0F);
                let color = TEXT_PALETTE_RGBA[bg];

                let px_x0 = col * TEXT_MODE_CHAR_WIDTH;
                let px_y0 = row * TEXT_MODE_CHAR_HEIGHT;

                for y in 0..TEXT_MODE_CHAR_HEIGHT {
                    let base = (px_y0 + y) * TEXT_MODE_WIDTH + px_x0;
                    self.framebuffer[base..base + TEXT_MODE_CHAR_WIDTH].fill(color);
                }
            }
        }

        &self.framebuffer
    }
}
