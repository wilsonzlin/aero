use aero_gpu_vga::FONT8X8_CP437;
use firmware::bda::BiosDataArea;
use memory::MemoryBus;

// VGA text modes use a 9x16 cell on the standard VGA timing (720x400).
const TEXT_CHAR_WIDTH: usize = 9;
const TEXT_CHAR_HEIGHT: usize = 16;

// Defensive caps: the BIOS text renderer is intended for 80x25 mode and should not allocate
// arbitrarily large buffers based on guest-controlled BDA values.
const MAX_TEXT_COLS: u16 = 80;
const MAX_TEXT_ROWS: u8 = 25;

fn vga_color(
    dac_palette: &[[u8; 3]; 256],
    pel_mask: u8,
    attr_4bit: u8,
    attr_regs: &[u8; 256],
) -> u32 {
    let idx = vga_attribute_palette_lookup(attr_4bit, attr_regs) & pel_mask;
    let [r6, g6, b6] = dac_palette[idx as usize];
    let r = vga_6bit_to_8bit(r6);
    let g = vga_6bit_to_8bit(g6);
    let b = vga_6bit_to_8bit(b6);
    u32::from_le_bytes([r, g, b, 0xFF])
}

fn vga_attribute_palette_lookup(color: u8, attr_regs: &[u8; 256]) -> u8 {
    // Attribute Controller indices.
    const MODE_CONTROL: usize = 0x10;
    const COLOR_PLANE_ENABLE: usize = 0x12;
    const COLOR_SELECT: usize = 0x14;

    // Mirror the VGA Attribute Controller palette mapping logic:
    // - Color Plane Enable masks the 4-bit color index.
    // - Palette registers provide a 6-bit "PEL" (0..=63).
    // - When the Mode Control P54S bit is set, palette bits 5-4 are sourced from
    //   Color Select bits 3-2 instead of the palette register.
    // - The top 2 bits of the DAC index (7-6) come from Color Select bits 1-0.
    let mode_control = attr_regs[MODE_CONTROL];
    let color_plane_enable = attr_regs[COLOR_PLANE_ENABLE] & 0x0F;
    let color_select = attr_regs[COLOR_SELECT];

    let masked = (color & 0x0F) & color_plane_enable;
    let mut pel = attr_regs[masked as usize] & 0x3F;
    if (mode_control & 0x80) != 0 {
        pel = (pel & 0x0F) | ((color_select & 0x0C) << 2);
    }
    ((color_select & 0x03) << 6) | pel
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
/// CRTC ports. Instead of mirroring BDA state into VGA registers, we render directly from BDA for
/// cursor/page state, but use guest-programmed VGA palette state (DAC + Attribute Controller) for
/// color mapping:
/// - Visible text page base = `0xB8000 + BDA.video_page_offset`
/// - Cursor position/shape = `BDA.active_page`, `BDA.cursor_pos[page]`, `BDA.cursor_shape`
pub fn render_into(
    fb: &mut Vec<u32>,
    mem: &mut impl MemoryBus,
    dac_palette: &[[u8; 3]; 256],
    pel_mask: u8,
    attr_regs: &[u8; 256],
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

            let fg = vga_color(dac_palette, pel_mask, attr & 0x0F, attr_regs);
            let bg = vga_color(dac_palette, pel_mask, (attr >> 4) & 0x0F, attr_regs);

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
            let fg = vga_color(dac_palette, pel_mask, attr & 0x0F, attr_regs);
            let bg = vga_color(dac_palette, pel_mask, (attr >> 4) & 0x0F, attr_regs);

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
