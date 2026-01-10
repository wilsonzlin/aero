use font8x8::{BASIC_FONTS, UnicodeFonts};

use crate::devices::vga::dac::VgaDac;
use crate::devices::vga::memory::{VgaMemory, VramPlane};
use crate::devices::vga::ports::VgaDevice;

pub const TEXT_MODE_COLUMNS: usize = 80;
pub const TEXT_MODE_ROWS: usize = 25;

// VGA text modes use a 9x16 cell on the standard VGA timing (720x400).
pub const TEXT_MODE_CHAR_WIDTH: usize = 9;
pub const TEXT_MODE_CHAR_HEIGHT: usize = 16;

pub const TEXT_MODE_WIDTH: usize = TEXT_MODE_COLUMNS * TEXT_MODE_CHAR_WIDTH;
pub const TEXT_MODE_HEIGHT: usize = TEXT_MODE_ROWS * TEXT_MODE_CHAR_HEIGHT;
pub const TEXT_MODE_FRAMEBUFFER_SIZE: usize = TEXT_MODE_WIDTH * TEXT_MODE_HEIGHT;

#[derive(Debug)]
pub struct TextModeRenderer {
    framebuffer: Vec<u32>,
}

impl TextModeRenderer {
    pub fn new() -> Self {
        Self {
            framebuffer: vec![0; TEXT_MODE_FRAMEBUFFER_SIZE],
        }
    }

    pub fn render<'a>(
        &'a mut self,
        regs: &VgaDevice,
        vram: &mut VgaMemory,
        dac: &mut VgaDac,
    ) -> &'a [u32] {
        // Text mode updates are typically sparse and tied to CPU writes. For now we keep the
        // renderer simple and repaint the full frame whenever asked.
        let _ = vram.take_dirty_plane_pages();
        let _ = vram.take_dirty_pages();
        let _ = dac.take_dirty();

        let plane0 = vram.plane(VramPlane(0));
        let plane1 = vram.plane(VramPlane(1));
        let pel_mask = dac.pel_mask();
        let palette = dac.palette_rgba();

        let cursor_pos =
            ((regs.crtc_regs.get(0x0E).copied().unwrap_or(0) as u16) << 8)
                | regs.crtc_regs.get(0x0F).copied().unwrap_or(0) as u16;
        let cursor_start = regs.crtc_regs.get(0x0A).copied().unwrap_or(0) & 0x1F;
        let cursor_end = regs.crtc_regs.get(0x0B).copied().unwrap_or(0) & 0x1F;
        let cursor_disabled = (regs.crtc_regs.get(0x0A).copied().unwrap_or(0) & 0x20) != 0;

        for row in 0..TEXT_MODE_ROWS {
            for col in 0..TEXT_MODE_COLUMNS {
                let cell = row * TEXT_MODE_COLUMNS + col;
                let ch = plane0.get(cell).copied().unwrap_or(0);
                let attr = plane1.get(cell).copied().unwrap_or(0);

                let fg_raw = attr & 0x0F;
                let bg_raw = (attr >> 4) & 0x0F;

                let fg_idx = map_attribute_controller(regs, fg_raw) & pel_mask;
                let bg_idx = map_attribute_controller(regs, bg_raw) & pel_mask;

                let fg = palette[fg_idx as usize];
                let bg = palette[bg_idx as usize];

                let draw_glyph = regs.should_render_text_attribute(attr);

                let px_x0 = col * TEXT_MODE_CHAR_WIDTH;
                let px_y0 = row * TEXT_MODE_CHAR_HEIGHT;

                for gy in 0..TEXT_MODE_CHAR_HEIGHT {
                    let bits = if draw_glyph { glyph8x16_row(ch, gy) } else { 0 };
                    for gx in 0..TEXT_MODE_CHAR_WIDTH {
                        let on = if gx < 8 {
                            ((bits >> (7 - gx)) & 1) != 0
                        } else {
                            let is_line = (0xC0..=0xDF).contains(&ch);
                            is_line && (bits & 0x01) != 0
                        };
                        let dst_x = px_x0 + gx;
                        let dst_y = px_y0 + gy;
                        let idx = dst_y * TEXT_MODE_WIDTH + dst_x;
                        self.framebuffer[idx] = if on { fg } else { bg };
                    }
                }

                if !cursor_disabled && cell as u16 == cursor_pos {
                    let start = cursor_start.min(15) as usize;
                    let end = cursor_end.min(15) as usize;
                    for gy in start..=end {
                        let dst_y = px_y0 + gy;
                        let base = dst_y * TEXT_MODE_WIDTH + px_x0;
                        self.framebuffer[base..base + TEXT_MODE_CHAR_WIDTH].fill(fg);
                    }
                }
            }
        }

        &self.framebuffer
    }
}

fn glyph8x16_row(ch: u8, row: usize) -> u8 {
    let glyph8 = BASIC_FONTS.get(ch as char).unwrap_or([0; 8]);
    glyph8[row / 2]
}

fn map_attribute_controller(regs: &VgaDevice, index: u8) -> u8 {
    // Attribute Controller indices.
    const MODE_CONTROL: usize = 0x10;
    const COLOR_PLANE_ENABLE: usize = 0x12;
    const COLOR_SELECT: usize = 0x14;

    let mode_control = regs.ac_regs.get(MODE_CONTROL).copied().unwrap_or(0);
    let color_plane_enable = regs.ac_regs.get(COLOR_PLANE_ENABLE).copied().unwrap_or(0x0F);
    let color_select = regs.ac_regs.get(COLOR_SELECT).copied().unwrap_or(0);

    let masked = index & (color_plane_enable & 0x0F);

    // Palette entry is 6-bit (0..=63).
    let mut pel = regs.ac_regs.get(masked as usize).copied().unwrap_or(0) & 0x3F;

    // VGA "Palette bits 5-4 select" (P54S): when set, bits 5-4 of the palette entry come from
    // Color Select bits 3-2 instead of the palette register.
    if (mode_control & 0x80) != 0 {
        pel = (pel & 0x0F) | ((color_select & 0x0C) << 2);
    }

    // Bits 7-6 of the final DAC index come from Color Select bits 1-0.
    ((color_select & 0x03) << 6) | pel
}
