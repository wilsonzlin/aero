use crate::devices::vga::dac::VgaDac;
use crate::devices::vga::memory::{VgaMemory, VramPlane};
use crate::devices::vga::VgaDevice;

pub const MODE12H_WIDTH: usize = 640;
pub const MODE12H_HEIGHT: usize = 480;
pub const MODE12H_BYTES_PER_SCANLINE: usize = MODE12H_WIDTH / 8;

#[derive(Debug)]
pub struct Mode12hRenderer {
    framebuffer: Vec<u32>,
}

impl Mode12hRenderer {
    pub fn new() -> Self {
        Self {
            framebuffer: vec![0u32; MODE12H_WIDTH * MODE12H_HEIGHT],
        }
    }

    pub fn render<'a>(
        &'a mut self,
        regs: &VgaDevice,
        vram: &mut VgaMemory,
        dac: &mut VgaDac,
    ) -> &'a [u32] {
        // Mode 12h is typically programmed with full-screen updates (drawing primitives into VRAM).
        // For now we keep it simple and always repaint; optimizations can come later.
        let _ = vram.take_dirty_pages();
        let _ = dac.take_dirty();
        self.render_full(regs, vram, dac);
        &self.framebuffer
    }

    fn render_full(&mut self, regs: &VgaDevice, vram: &VgaMemory, dac: &VgaDac) {
        let plane0 = vram.plane(VramPlane(0));
        let plane1 = vram.plane(VramPlane(1));
        let plane2 = vram.plane(VramPlane(2));
        let plane3 = vram.plane(VramPlane(3));
        let pel_mask = dac.pel_mask();
        let palette = dac.palette_rgba();

        for y in 0..MODE12H_HEIGHT {
            let row_offset = y * MODE12H_BYTES_PER_SCANLINE;
            for byte_x in 0..MODE12H_BYTES_PER_SCANLINE {
                let offset = row_offset + byte_x;

                let p0 = plane0[offset];
                let p1 = plane1[offset];
                let p2 = plane2[offset];
                let p3 = plane3[offset];

                for bit in 0..8 {
                    let bit_index = 7 - bit;
                    let raw_index = ((p0 >> bit_index) & 1)
                        | (((p1 >> bit_index) & 1) << 1)
                        | (((p2 >> bit_index) & 1) << 2)
                        | (((p3 >> bit_index) & 1) << 3);

                    let dac_index = map_attribute_controller(regs, raw_index as u8) & pel_mask;
                    let pixel = palette[dac_index as usize];

                    let x = byte_x * 8 + bit;
                    self.framebuffer[y * MODE12H_WIDTH + x] = pixel;
                }
            }
        }
    }
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
