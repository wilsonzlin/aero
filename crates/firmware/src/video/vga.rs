use crate::bda::BiosDataArea;
use crate::memory::MemoryBus;

#[derive(Debug, Clone)]
pub struct VgaDevice {
    text_base: u64,
    default_attr: u8,
}

impl VgaDevice {
    pub fn new() -> Self {
        Self {
            text_base: 0xB8000,
            default_attr: 0x07,
        }
    }

    pub fn set_text_mode_03h(&mut self, mem: &mut impl MemoryBus) {
        BiosDataArea::write_video_mode(mem, 0x03);
        BiosDataArea::write_screen_cols(mem, 80);
        BiosDataArea::write_page_size(mem, 80 * 25 * 2);
        BiosDataArea::write_active_page(mem, 0);
        BiosDataArea::write_cursor_pos_page0(mem, 0, 0);
        BiosDataArea::write_cursor_shape(mem, 0x06, 0x07);

        self.clear_text_buffer(mem, 0x07);
    }

    pub fn set_cursor_pos(&mut self, mem: &mut impl MemoryBus, page: u8, row: u8, col: u8) {
        if page != 0 {
            return;
        }
        BiosDataArea::write_cursor_pos_page0(mem, row, col);
    }

    pub fn get_cursor_pos(&self, mem: &impl MemoryBus, page: u8) -> (u8, u8) {
        if page != 0 {
            return (0, 0);
        }
        BiosDataArea::read_cursor_pos_page0(mem)
    }

    pub fn set_cursor_shape(&mut self, mem: &mut impl MemoryBus, start: u8, end: u8) {
        BiosDataArea::write_cursor_shape(mem, start, end);
    }

    pub fn get_cursor_shape(&self, mem: &impl MemoryBus) -> (u8, u8) {
        BiosDataArea::read_cursor_shape(mem)
    }

    pub fn teletype_output(&mut self, mem: &mut impl MemoryBus, page: u8, ch: u8, attr: u8) {
        if page != 0 {
            return;
        }

        let cols = BiosDataArea::read_screen_cols(mem) as u8;
        let rows = 25u8;
        let (mut row, mut col) = BiosDataArea::read_cursor_pos_page0(mem);

        match ch {
            b'\r' => {
                col = 0;
            }
            b'\n' => {
                row = row.saturating_add(1);
            }
            0x08 => {
                // backspace
                col = col.saturating_sub(1);
            }
            ch => {
                self.write_text_cell(
                    mem,
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
            self.scroll_up(mem, 0, 1, self.default_attr, 0, 0, rows - 1, cols - 1);
            row = rows - 1;
        }

        BiosDataArea::write_cursor_pos_page0(mem, row, col);
    }

    pub fn scroll_up(
        &mut self,
        mem: &mut impl MemoryBus,
        page: u8,
        lines: u8,
        attr: u8,
        top_row: u8,
        top_col: u8,
        bottom_row: u8,
        bottom_col: u8,
    ) {
        if page != 0 {
            return;
        }

        let cols = BiosDataArea::read_screen_cols(mem) as u16;
        let rows = 25u16;
        let top_row = top_row as u16;
        let top_col = top_col as u16;
        let bottom_row = bottom_row.min((rows - 1) as u8) as u16;
        let bottom_col = bottom_col.min((cols - 1) as u8) as u16;

        let lines = lines as u16;
        let window_rows = bottom_row - top_row + 1;
        let scroll_lines = if lines == 0 || lines > window_rows {
            window_rows
        } else {
            lines
        };

        for row in 0..window_rows {
            let src_row = row + scroll_lines;
            for col in 0..=(bottom_col - top_col) {
                let dst_r = top_row + row;
                let dst_c = top_col + col;

                if src_row < window_rows {
                    let src_r = top_row + src_row;
                    let src_c = top_col + col;
                    let src_off = self.text_offset(cols, src_r, src_c);
                    let ch = mem.read_u8(self.text_base + src_off);
                    let at = mem.read_u8(self.text_base + src_off + 1);
                    let dst_off = self.text_offset(cols, dst_r, dst_c);
                    mem.write_u8(self.text_base + dst_off, ch);
                    mem.write_u8(self.text_base + dst_off + 1, at);
                } else {
                    let dst_off = self.text_offset(cols, dst_r, dst_c);
                    mem.write_u8(self.text_base + dst_off, b' ');
                    mem.write_u8(self.text_base + dst_off + 1, attr);
                }
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
        if page != 0 {
            return;
        }

        let cols = BiosDataArea::read_screen_cols(mem) as u8;
        let rows = 25u8;
        let (mut row, mut col) = BiosDataArea::read_cursor_pos_page0(mem);

        for _ in 0..count {
            self.write_text_cell(mem, row, col, ch, attr);
            col = col.wrapping_add(1);
            if col >= cols {
                col = 0;
                row = row.wrapping_add(1);
            }
            if row >= rows {
                self.scroll_up(mem, 0, 1, self.default_attr, 0, 0, rows - 1, cols - 1);
                row = rows - 1;
            }
        }

        BiosDataArea::write_cursor_pos_page0(mem, row, col);
    }

    fn clear_text_buffer(&self, mem: &mut impl MemoryBus, attr: u8) {
        let cols = 80u32;
        let rows = 25u32;
        for cell in 0..(cols * rows) {
            let addr = self.text_base + (cell * 2) as u64;
            mem.write_u8(addr, b' ');
            mem.write_u8(addr + 1, attr);
        }
    }

    fn write_text_cell(&self, mem: &mut impl MemoryBus, row: u8, col: u8, ch: u8, attr: u8) {
        let cols = BiosDataArea::read_screen_cols(mem) as u16;
        let off = self.text_offset(cols, row as u16, col as u16);
        mem.write_u8(self.text_base + off, ch);
        mem.write_u8(self.text_base + off + 1, attr);
    }

    fn text_offset(&self, cols: u16, row: u16, col: u16) -> u64 {
        ((row * cols + col) * 2) as u64
    }
}
