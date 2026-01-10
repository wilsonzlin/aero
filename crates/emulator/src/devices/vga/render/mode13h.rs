use crate::devices::vga::dac::VgaDac;
use crate::devices::vga::memory::VgaMemory;

pub const MODE13H_WIDTH: usize = 320;
pub const MODE13H_HEIGHT: usize = 200;
pub const MODE13H_VRAM_SIZE: usize = MODE13H_WIDTH * MODE13H_HEIGHT;
pub const MODE13H_VRAM_TOTAL_PAGES: usize = 16; // 64k / 4k

const BYTES_PER_SCANLINE: usize = MODE13H_WIDTH;
const PAGE_SIZE: usize = 4096;
const FULL_REPAINT_DIRTY_PAGE_THRESHOLD: usize = 8;

#[derive(Debug)]
pub struct Mode13hRenderer {
    framebuffer: Vec<u32>,
    full_repaint_requested: bool,
}

impl Mode13hRenderer {
    pub fn new() -> Self {
        Self {
            framebuffer: vec![0u32; MODE13H_VRAM_SIZE],
            full_repaint_requested: true,
        }
    }

    #[inline]
    pub fn request_full_repaint(&mut self) {
        self.full_repaint_requested = true;
    }

    pub fn render<'a>(&'a mut self, vram: &mut VgaMemory, dac: &mut VgaDac) -> &'a [u32] {
        let palette_dirty = dac.take_dirty();
        let dirty_pages_mask =
            vram.take_dirty_pages() & ((1u64 << MODE13H_VRAM_TOTAL_PAGES) - 1u64);

        if palette_dirty {
            self.full_repaint_requested = true;
        }

        if self.full_repaint_requested {
            self.full_repaint_requested = false;
            self.render_full(vram, dac);
            return &self.framebuffer;
        }

        if dirty_pages_mask == 0 {
            return &self.framebuffer;
        }

        let dirty_page_count = dirty_pages_mask.count_ones() as usize;
        if dirty_page_count >= FULL_REPAINT_DIRTY_PAGE_THRESHOLD {
            self.render_full(vram, dac);
            return &self.framebuffer;
        }

        self.render_partial(vram, dac, dirty_pages_mask);
        &self.framebuffer
    }

    fn render_full(&mut self, vram: &VgaMemory, dac: &VgaDac) {
        let src = vram.data();
        let pel_mask = dac.pel_mask();
        let palette = dac.palette_rgba();

        for (dst, &pixel) in self
            .framebuffer
            .iter_mut()
            .zip(src[..MODE13H_VRAM_SIZE].iter())
        {
            let index = pixel & pel_mask;
            *dst = palette[index as usize];
        }
    }

    fn render_partial(&mut self, vram: &VgaMemory, dac: &VgaDac, dirty_pages_mask: u64) {
        let src = vram.data();
        let pel_mask = dac.pel_mask();
        let palette = dac.palette_rgba();

        let mut dirty_scanlines = [false; MODE13H_HEIGHT];

        for page in 0..MODE13H_VRAM_TOTAL_PAGES {
            if (dirty_pages_mask & (1u64 << page)) == 0 {
                continue;
            }

            let start = page * PAGE_SIZE;
            let end = (start + PAGE_SIZE).min(MODE13H_VRAM_SIZE);

            let start_line = start / BYTES_PER_SCANLINE;
            let end_line = (end - 1) / BYTES_PER_SCANLINE;

            for y in start_line..=end_line {
                if y < MODE13H_HEIGHT {
                    dirty_scanlines[y] = true;
                }
            }
        }

        for y in 0..MODE13H_HEIGHT {
            if !dirty_scanlines[y] {
                continue;
            }

            let src_row = &src[y * BYTES_PER_SCANLINE..(y + 1) * BYTES_PER_SCANLINE];
            let dst_row =
                &mut self.framebuffer[y * BYTES_PER_SCANLINE..(y + 1) * BYTES_PER_SCANLINE];

            for (dst, &pixel) in dst_row.iter_mut().zip(src_row.iter()) {
                let index = pixel & pel_mask;
                *dst = palette[index as usize];
            }
        }
    }
}
