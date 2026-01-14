use firmware::bda::BiosDataArea;
use aero_gpu_vga::FONT8X8_CP437;
use memory::MemoryBus;

// VGA text modes use a 9x16 cell on the standard VGA timing (720x400).
const TEXT_CHAR_WIDTH: usize = 9;
const TEXT_CHAR_HEIGHT: usize = 16;

// Defensive caps: the BIOS text renderer is intended for 80x25 mode and should not allocate
// arbitrarily large buffers based on guest-controlled BDA values.
const MAX_TEXT_COLS: u16 = 80;
const MAX_TEXT_ROWS: u8 = 25;

fn vga_color(dac_palette: &[[u8; 3]; 256], pel_mask: u8, attr_4bit: u8) -> u32 {
    // VGA applies `PEL_MASK` before palette lookup.
    let idx = (attr_4bit & 0x0F) & pel_mask;
    let [r6, g6, b6] = dac_palette[idx as usize];
    let r = vga_6bit_to_8bit(r6);
    let g = vga_6bit_to_8bit(g6);
    let b = vga_6bit_to_8bit(b6);
    u32::from_le_bytes([r, g, b, 0xFF])
}

fn vga_6bit_to_8bit(v: u8) -> u8 {
    let v = v & 0x3F;
    // Expand 6-bit DAC component to 8-bit (matches the VGA model's `palette::vga_6bit_to_8bit`).
    (v << 2) | (v >> 4)
}

fn glyph8x16_row(ch: u8, row: usize) -> u8 {
    FONT8X8_CP437[ch as usize][row / 2]
}

/// Render a VGA 80x25-style text mode framebuffer using BIOS Data Area (BDA) state as the source of
/// truth.
///
/// This is used for the AeroGPU "legacy text scanout" path where the HLE BIOS does not program VGA
/// CRTC ports. Instead of mirroring BDA state into VGA registers, we render directly from BDA:
/// - Visible text page base = `0xB8000 + BDA.video_page_offset`
/// - Cursor position/shape = `BDA.active_page`, `BDA.cursor_pos[page]`, `BDA.cursor_shape`
pub fn render_into(
    fb: &mut Vec<u32>,
    mem: &mut impl MemoryBus,
    dac_palette: &[[u8; 3]; 256],
    pel_mask: u8,
) -> (u32, u32) {
    let cols = BiosDataArea::read_screen_cols(mem).clamp(1, MAX_TEXT_COLS) as usize;
    let rows = BiosDataArea::read_text_rows(mem).clamp(1, MAX_TEXT_ROWS) as usize;

    let width = cols * TEXT_CHAR_WIDTH;
    let height = rows * TEXT_CHAR_HEIGHT;

    fb.resize(width.saturating_mul(height), 0);

    let page_offset = u64::from(BiosDataArea::read_video_page_offset(mem));
    let base = 0xB8000u64 + page_offset;

    // Render glyphs + attributes.
    for row in 0..rows {
        for col in 0..cols {
            let cell = row * cols + col;
            let addr = base + (cell as u64) * 2;
            let ch = mem.read_u8(addr);
            let attr = mem.read_u8(addr + 1);

            let fg = vga_color(dac_palette, pel_mask, attr & 0x0F);
            let bg = vga_color(dac_palette, pel_mask, (attr >> 4) & 0x0F);

            let px_x0 = col * TEXT_CHAR_WIDTH;
            let px_y0 = row * TEXT_CHAR_HEIGHT;

            for gy in 0..TEXT_CHAR_HEIGHT {
                let bits = glyph8x16_row(ch, gy);
                for gx in 0..TEXT_CHAR_WIDTH {
                    let on = if gx < 8 {
                        ((bits >> (7 - gx)) & 1) != 0
                    } else {
                        let is_line = (0xC0..=0xDF).contains(&ch);
                        is_line && (bits & 0x01) != 0
                    };
                    let dst_x = px_x0 + gx;
                    let dst_y = px_y0 + gy;
                    fb[dst_y * width + dst_x] = if on { fg } else { bg };
                }
            }
        }
    }

    // Cursor overlay.
    let cursor_page = BiosDataArea::read_active_page(mem);
    let (cursor_row, cursor_col) = BiosDataArea::read_cursor_pos(mem, cursor_page);
    let (cursor_start, cursor_end) = BiosDataArea::read_cursor_shape(mem);

    // Cursor disable semantics match VGA CRTC: if bit 5 of the start value is set, the cursor is
    // disabled.
    let cursor_disabled = (cursor_start & 0x20) != 0;

    if !cursor_disabled {
        let row = cursor_row as usize;
        let col = cursor_col as usize;
        if row < rows && col < cols {
            let cell = row * cols + col;
            let addr = base + (cell as u64) * 2;
            let attr = mem.read_u8(addr + 1);
            let fg = vga_color(dac_palette, pel_mask, attr & 0x0F);
            let bg = vga_color(dac_palette, pel_mask, (attr >> 4) & 0x0F);

            let start = (cursor_start & 0x1F).min((TEXT_CHAR_HEIGHT - 1) as u8) as usize;
            let end = (cursor_end & 0x1F).min((TEXT_CHAR_HEIGHT - 1) as u8) as usize;
            if start <= end {
                let px_x0 = col * TEXT_CHAR_WIDTH;
                let px_y0 = row * TEXT_CHAR_HEIGHT;
                for gy in start..=end {
                    let dst_y = px_y0 + gy;
                    let base = dst_y * width + px_x0;
                    for px in &mut fb[base..base + TEXT_CHAR_WIDTH] {
                        *px = if *px == fg { bg } else { fg };
                    }
                }
            }
        }
    }

    (width as u32, height as u32)
}
