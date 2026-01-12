use crate::bda::BiosDataArea;
use crate::memory::MemoryBus;

#[derive(Debug, Clone)]
pub struct VgaDevice {
    text_base: u64,
    default_attr: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextWindow {
    pub top_row: u8,
    pub top_col: u8,
    pub bottom_row: u8,
    pub bottom_col: u8,
}

impl VgaDevice {
    fn text_base_for_page(&self, mem: &mut impl MemoryBus, page: u8) -> u64 {
        if page >= 8 {
            return self.text_base;
        }
        let page_size = BiosDataArea::read_page_size(mem) as u64;
        self.text_base + page_size.saturating_mul(u64::from(page))
    }

    pub fn new() -> Self {
        Self {
            text_base: 0xB8000,
            default_attr: 0x07,
        }
    }

    pub fn set_text_mode_03h(&mut self, mem: &mut impl MemoryBus, clear: bool) {
        BiosDataArea::write_video_mode(mem, 0x03);
        BiosDataArea::write_screen_cols(mem, 80);
        BiosDataArea::write_text_rows(mem, 25);
        BiosDataArea::write_page_size(mem, 80 * 25 * 2);
        BiosDataArea::write_video_page_offset(mem, 0);
        BiosDataArea::write_active_page(mem, 0);
        // Color CRTC base I/O port.
        BiosDataArea::write_crtc_base(mem, 0x3D4);
        for page in 0..8u8 {
            BiosDataArea::write_cursor_pos(mem, page, 0, 0);
        }
        BiosDataArea::write_cursor_shape(mem, 0x06, 0x07);

        if clear {
            self.clear_text_buffer(mem, 0x07);
        }
    }

    pub fn set_cursor_pos(&mut self, mem: &mut impl MemoryBus, page: u8, row: u8, col: u8) {
        let rows = BiosDataArea::read_text_rows(mem).max(1);
        let cols = BiosDataArea::read_screen_cols(mem).max(1) as u8;
        let row = row.min(rows.saturating_sub(1));
        let col = col.min(cols.saturating_sub(1));
        BiosDataArea::write_cursor_pos(mem, page, row, col);
    }

    pub fn get_cursor_pos(&self, mem: &mut impl MemoryBus, page: u8) -> (u8, u8) {
        BiosDataArea::read_cursor_pos(mem, page)
    }

    pub fn set_cursor_shape(&mut self, mem: &mut impl MemoryBus, start: u8, end: u8) {
        BiosDataArea::write_cursor_shape(mem, start, end);
    }

    pub fn get_cursor_shape(&self, mem: &mut impl MemoryBus) -> (u8, u8) {
        BiosDataArea::read_cursor_shape(mem)
    }

    pub fn teletype_output(&mut self, mem: &mut impl MemoryBus, page: u8, ch: u8, attr: u8) {
        let cols = BiosDataArea::read_screen_cols(mem) as u8;
        let rows = BiosDataArea::read_text_rows(mem).max(1);
        let (mut row, mut col) = BiosDataArea::read_cursor_pos(mem, page);
        let base = self.text_base_for_page(mem, page);

        match ch {
            b'\r' => {
                col = 0;
            }
            b'\n' => {
                row = row.saturating_add(1);
            }
            0x08 => {
                // backspace
                if col > 0 {
                    col = col.saturating_sub(1);
                } else if row > 0 {
                    row = row.saturating_sub(1);
                    col = cols.saturating_sub(1);
                }
            }
            ch => {
                self.write_text_cell_at_base(
                    mem,
                    base,
                    row,
                    col,
                    ch,
                    if attr == 0 { self.default_attr } else { attr },
                );
                col = col.wrapping_add(1);
                if col >= cols {
                    col = 0;
                    row = row.wrapping_add(1);
                }
            }
        }

        if row >= rows {
            // Scroll up one line and keep cursor on last line.
            self.scroll_up(
                mem,
                page,
                1,
                self.default_attr,
                TextWindow {
                    top_row: 0,
                    top_col: 0,
                    bottom_row: rows - 1,
                    bottom_col: cols - 1,
                },
            );
            row = rows - 1;
        }

        BiosDataArea::write_cursor_pos(mem, page, row, col);
    }

    pub fn scroll_up(
        &mut self,
        mem: &mut impl MemoryBus,
        page: u8,
        lines: u8,
        attr: u8,
        window: TextWindow,
    ) {
        let cols = BiosDataArea::read_screen_cols(mem).max(1);
        let rows = u16::from(BiosDataArea::read_text_rows(mem).max(1));
        let base = self.text_base_for_page(mem, page);

        let bottom_row = u16::from(window.bottom_row).min(rows.saturating_sub(1));
        let bottom_col = u16::from(window.bottom_col).min(cols.saturating_sub(1));
        let top_row = u16::from(window.top_row)
            .min(rows.saturating_sub(1))
            .min(bottom_row);
        let top_col = u16::from(window.top_col)
            .min(cols.saturating_sub(1))
            .min(bottom_col);

        let lines = lines as u16;
        let window_rows = bottom_row - top_row + 1;
        let scroll_lines = if lines == 0 || lines > window_rows {
            window_rows
        } else {
            lines
        };

        let window_cols = bottom_col - top_col + 1;
        let row_bytes = (window_cols as usize).saturating_mul(2);
        if row_bytes == 0 {
            return;
        }

        let mut buf = vec![0u8; row_bytes];
        let mut blank_ready = false;
        for row in 0..window_rows {
            let dst_r = top_row + row;
            let dst_off = self.text_offset(cols, dst_r, top_col);
            let dst_addr = base + dst_off;

            let src_r = dst_r + scroll_lines;
            if src_r <= bottom_row {
                let src_off = self.text_offset(cols, src_r, top_col);
                let src_addr = base + src_off;
                mem.read_physical(src_addr, &mut buf);
                mem.write_physical(dst_addr, &buf);
            } else {
                if !blank_ready {
                    for pair in buf.chunks_exact_mut(2) {
                        pair[0] = b' ';
                        pair[1] = attr;
                    }
                    blank_ready = true;
                }
                mem.write_physical(dst_addr, &buf);
            }
        }
    }

    pub fn scroll_down(
        &mut self,
        mem: &mut impl MemoryBus,
        page: u8,
        lines: u8,
        attr: u8,
        window: TextWindow,
    ) {
        let cols = BiosDataArea::read_screen_cols(mem).max(1);
        let rows = u16::from(BiosDataArea::read_text_rows(mem).max(1));
        let base = self.text_base_for_page(mem, page);

        let bottom_row = u16::from(window.bottom_row).min(rows.saturating_sub(1));
        let bottom_col = u16::from(window.bottom_col).min(cols.saturating_sub(1));
        let top_row = u16::from(window.top_row)
            .min(rows.saturating_sub(1))
            .min(bottom_row);
        let top_col = u16::from(window.top_col)
            .min(cols.saturating_sub(1))
            .min(bottom_col);

        let lines = lines as u16;
        let window_rows = bottom_row - top_row + 1;
        let scroll_lines = if lines == 0 || lines > window_rows {
            window_rows
        } else {
            lines
        };

        let window_cols = bottom_col - top_col + 1;
        let row_bytes = (window_cols as usize).saturating_mul(2);
        if row_bytes == 0 {
            return;
        }

        let mut buf = vec![0u8; row_bytes];
        let mut blank_ready = false;

        // Iterate bottom-up so we don't overwrite the source rows before copying.
        for row in (0..window_rows).rev() {
            let dst_r = top_row + row;
            let dst_off = self.text_offset(cols, dst_r, top_col);
            let dst_addr = base + dst_off;

            if row >= scroll_lines {
                let src_r = top_row + row - scroll_lines;
                let src_off = self.text_offset(cols, src_r, top_col);
                let src_addr = base + src_off;
                mem.read_physical(src_addr, &mut buf);
                mem.write_physical(dst_addr, &buf);
            } else {
                if !blank_ready {
                    for pair in buf.chunks_exact_mut(2) {
                        pair[0] = b' ';
                        pair[1] = attr;
                    }
                    blank_ready = true;
                }
                mem.write_physical(dst_addr, &buf);
            }
        }
    }

    pub fn write_char_attr(
        &mut self,
        mem: &mut impl MemoryBus,
        page: u8,
        ch: u8,
        attr: u8,
        count: u16,
    ) {
        if count == 0 {
            return;
        }

        let cols = BiosDataArea::read_screen_cols(mem).max(1) as u8;
        let rows = BiosDataArea::read_text_rows(mem).max(1);
        let (row0, col0) = BiosDataArea::read_cursor_pos(mem, page);
        let base = self.text_base_for_page(mem, page);

        let mut linear = row0 as u32 * cols as u32 + col0 as u32;
        let max = rows as u32 * cols as u32;
        for _ in 0..count {
            if linear >= max {
                break;
            }
            let row = (linear / cols as u32) as u8;
            let col = (linear % cols as u32) as u8;
            self.write_text_cell_at_base(mem, base, row, col, ch, attr);
            linear += 1;
        }
    }

    pub fn write_char_only(&mut self, mem: &mut impl MemoryBus, page: u8, ch: u8, count: u16) {
        if count == 0 {
            return;
        }

        let cols = BiosDataArea::read_screen_cols(mem).max(1) as u8;
        let rows = BiosDataArea::read_text_rows(mem).max(1);
        let (row0, col0) = BiosDataArea::read_cursor_pos(mem, page);
        let base = self.text_base_for_page(mem, page);

        let mut linear = row0 as u32 * cols as u32 + col0 as u32;
        let max = rows as u32 * cols as u32;
        for _ in 0..count {
            if linear >= max {
                break;
            }
            let row = (linear / cols as u32) as u8;
            let col = (linear % cols as u32) as u8;
            let off = self.text_offset(cols as u16, row as u16, col as u16);
            let attr = mem.read_u8(base + off + 1);
            mem.write_u8(base + off, ch);
            mem.write_u8(base + off + 1, attr);
            linear += 1;
        }
    }

    pub fn read_char_attr_at_cursor(&self, mem: &mut impl MemoryBus, page: u8) -> (u8, u8) {
        let (row, col) = BiosDataArea::read_cursor_pos(mem, page);
        let cols = BiosDataArea::read_screen_cols(mem).max(1);
        let base = self.text_base_for_page(mem, page);
        let off = self.text_offset(cols, row as u16, col as u16);
        let ch = mem.read_u8(base + off);
        let attr = mem.read_u8(base + off + 1);
        (ch, attr)
    }

    pub fn write_text_cell(
        &mut self,
        mem: &mut impl MemoryBus,
        page: u8,
        row: u8,
        col: u8,
        ch: u8,
        attr: u8,
    ) {
        let base = self.text_base_for_page(mem, page);
        self.write_text_cell_at_base(mem, base, row, col, ch, attr);
    }

    fn clear_text_buffer(&self, mem: &mut impl MemoryBus, attr: u8) {
        // Clear the full 32KiB text window (16k cells). This covers all BIOS text pages.
        //
        // Use a bulk `write_physical` path when available so clearing does not devolve into
        // byte-at-a-time MMIO operations when the text window is backed by an emulated VGA device.
        const BYTES: usize = 0x4000 * 2;
        const CHUNK_SIZE: usize = 4096;
        const _: () = assert!(CHUNK_SIZE % 2 == 0);

        let mut chunk = [0u8; CHUNK_SIZE];
        for pair in chunk.chunks_exact_mut(2) {
            pair[0] = b' ';
            pair[1] = attr;
        }

        let mut addr = self.text_base;
        let mut remaining = BYTES;
        while remaining != 0 {
            let len = remaining.min(CHUNK_SIZE);
            mem.write_physical(addr, &chunk[..len]);
            addr = addr.saturating_add(len as u64);
            remaining -= len;
        }
    }

    fn write_text_cell_at_base(
        &self,
        mem: &mut impl MemoryBus,
        base: u64,
        row: u8,
        col: u8,
        ch: u8,
        attr: u8,
    ) {
        let cols = BiosDataArea::read_screen_cols(mem);
        let off = self.text_offset(cols, row as u16, col as u16);
        mem.write_u8(base + off, ch);
        mem.write_u8(base + off + 1, attr);
    }

    fn text_offset(&self, cols: u16, row: u16, col: u16) -> u64 {
        ((row * cols + col) * 2) as u64
    }
}

impl Default for VgaDevice {
    fn default() -> Self {
        Self::new()
    }
}
