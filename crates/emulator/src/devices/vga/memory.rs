use crate::devices::vga::render::mode13h::{MODE13H_VRAM_SIZE, MODE13H_VRAM_TOTAL_PAGES};

const PAGE_SIZE: usize = 4096;

#[derive(Debug)]
pub struct VgaMemory {
    vram: Vec<u8>,
    dirty_page_mask: u16,
}

impl VgaMemory {
    pub fn new() -> Self {
        Self {
            vram: vec![0u8; MODE13H_VRAM_SIZE],
            dirty_page_mask: u16::MAX >> (16 - MODE13H_VRAM_TOTAL_PAGES),
        }
    }

    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.vram
    }

    pub fn write(&mut self, offset: usize, data: &[u8]) {
        if offset >= self.vram.len() || data.is_empty() {
            return;
        }

        let write_len = data.len().min(self.vram.len() - offset);
        self.vram[offset..offset + write_len].copy_from_slice(&data[..write_len]);

        self.mark_dirty_range(offset, write_len);
    }

    #[inline]
    pub fn mark_all_dirty(&mut self) {
        self.dirty_page_mask = u16::MAX >> (16 - MODE13H_VRAM_TOTAL_PAGES);
    }

    #[inline]
    fn mark_dirty_range(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }

        let start_page = offset / PAGE_SIZE;
        let end_page = (offset + len - 1) / PAGE_SIZE;
        for page in start_page..=end_page {
            if page >= MODE13H_VRAM_TOTAL_PAGES {
                break;
            }
            self.dirty_page_mask |= 1u16 << page;
        }
    }

    /// Returns and clears the bitmask of dirty pages.
    #[inline]
    pub fn take_dirty_pages(&mut self) -> u16 {
        let pages = self.dirty_page_mask;
        self.dirty_page_mask = 0;
        pages
    }
}

