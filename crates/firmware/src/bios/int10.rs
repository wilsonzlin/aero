use crate::{bda::BiosDataArea, cpu::CpuState, memory::MemoryBus};

use super::Bios;

impl Bios {
    pub fn handle_int10(&mut self, cpu: &mut CpuState, memory: &mut impl MemoryBus) {
        if cpu.ax() & 0xFF00 == 0x4F00 {
            self.handle_int10_vbe(cpu, memory);
            return;
        }

        match cpu.ah() {
            0x00 => {
                // Set Video Mode (AL = mode)
                let raw = cpu.al();
                let mode = raw & 0x7F;
                let clear = (raw & 0x80) == 0;
                if mode == 0x03 {
                    self.video.vbe.current_mode = None;
                    self.video.vga.set_text_mode_03h(memory, clear);
                    self.video_mode = 0x03;
                }
            }
            0x05 => {
                // Select Active Display Page (AL = page)
                let page = cpu.al();
                if page < 8 {
                    BiosDataArea::write_active_page(memory, page);
                }
            }
            0x0F => {
                // Get Current Video Mode
                let mode = BiosDataArea::read_video_mode(memory);
                let cols = BiosDataArea::read_screen_cols(memory) as u8;
                let page = BiosDataArea::read_active_page(memory);
                cpu.set_al(mode);
                cpu.set_ah(cols);
                cpu.set_bh(page);
            }
            0x02 => {
                // Set Cursor Position
                let page = cpu.bh();
                let row = cpu.dh();
                let col = cpu.dl();
                self.video.vga.set_cursor_pos(memory, page, row, col);
            }
            0x03 => {
                // Get Cursor Position and Shape
                let page = cpu.bh();
                let (row, col) = self.video.vga.get_cursor_pos(memory, page);
                let (start, end) = self.video.vga.get_cursor_shape(memory);
                cpu.set_dh(row);
                cpu.set_dl(col);
                cpu.set_ch(start);
                cpu.set_cl(end);
            }
            0x01 => {
                // Set Cursor Shape
                let start = cpu.ch();
                let end = cpu.cl();
                self.video.vga.set_cursor_shape(memory, start, end);
            }
            0x0E => {
                // Teletype output
                let page = cpu.bh();
                let ch = cpu.al();
                let attr = cpu.bl();
                self.video.vga.teletype_output(memory, page, ch, attr);
            }
            0x06 => {
                // Scroll up
                let lines = cpu.al();
                let attr = cpu.bh();
                let top_row = cpu.ch();
                let top_col = cpu.cl();
                let bottom_row = cpu.dh();
                let bottom_col = cpu.dl();
                let page = BiosDataArea::read_active_page(memory);
                self.video.vga.scroll_up(
                    memory,
                    page,
                    lines,
                    attr,
                    crate::video::vga::TextWindow {
                        top_row,
                        top_col,
                        bottom_row,
                        bottom_col,
                    },
                );
            }
            0x09 => {
                // Write Character and Attribute at Cursor
                let ch = cpu.al();
                let page = cpu.bh();
                let attr = cpu.bl();
                let count = cpu.cx();
                self.video
                    .vga
                    .write_char_attr(memory, page, ch, attr, count);
            }
            _ => {
                // Unhandled INT 10h function.
            }
        }
    }
}
