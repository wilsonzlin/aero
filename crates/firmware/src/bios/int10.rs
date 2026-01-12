use crate::{
    bda::BiosDataArea,
    cpu::CpuState,
    memory::{real_addr, MemoryBus},
};

use super::{Bios, BIOS_SEGMENT, VGA_FONT_8X16_OFFSET};

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
                    let page_size = BiosDataArea::read_page_size(memory);
                    let offset = page_size.saturating_mul(u16::from(page));
                    BiosDataArea::write_video_page_offset(memory, offset);
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
            0x07 => {
                // Scroll down
                let lines = cpu.al();
                let attr = cpu.bh();
                let top_row = cpu.ch();
                let top_col = cpu.cl();
                let bottom_row = cpu.dh();
                let bottom_col = cpu.dl();
                let page = BiosDataArea::read_active_page(memory);
                self.video.vga.scroll_down(
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
            0x08 => {
                // Read Character and Attribute at Cursor
                let page = cpu.bh();
                let (ch, attr) = self.video.vga.read_char_attr_at_cursor(memory, page);
                cpu.set_al(ch);
                cpu.set_ah(attr);
            }
            0x1A => {
                // Display Combination Code (VGA).
                //
                // DOS-era programs use this to detect VGA presence and determine whether the
                // active display is monochrome vs color. We model a single VGA adapter with an
                // analog color monitor.
                match cpu.al() {
                    // Get Display Combination Code.
                    0x00 => {
                        cpu.set_al(0x1A); // function supported
                        cpu.set_bl(0x08); // VGA + analog color display
                        cpu.set_bh(0x00); // no alternate display
                    }
                    // Set Display Combination Code (ignored, but report success).
                    0x01 => {
                        cpu.set_al(0x1A); // function supported
                        cpu.set_bl(0x08);
                        cpu.set_bh(0x00);
                    }
                    _ => {
                        // Unhandled subfunction.
                    }
                }
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
            0x0A => {
                // Write Character Only at Cursor
                let ch = cpu.al();
                let page = cpu.bh();
                let count = cpu.cx();
                self.video.vga.write_char_only(memory, page, ch, count);
            }
            0x11 => {
                // Character generator routines.
                //
                // The most common subfunction used by DOS-era software is AL=30h "Get Font
                // Information", which returns a pointer to the ROM font table.
                match cpu.al() {
                    0x30 => {
                        // Return the built-in 8x16 font table in the system BIOS ROM.
                        cpu.set_es(BIOS_SEGMENT);
                        cpu.set_bp(VGA_FONT_8X16_OFFSET);
                        cpu.set_cx(16); // bytes per character
                        cpu.set_dl(24); // rows - 1 (25 rows)
                    }
                    _ => {
                        // Unhandled subfunction.
                    }
                }
            }
            0x13 => {
                // Write String
                //
                // This is primarily used by DOS-era software to quickly render text with a single
                // BIOS call.
                //
                // We implement a subset for text mode:
                // - start row/col from DH/DL
                // - page from BH
                // - attribute from BL (or per-character attributes if AL bit1 is set)
                // - update cursor only if AL bit0 is set
                let mode = cpu.al();
                let page = cpu.bh();
                let attr_default = cpu.bl();
                let count = cpu.cx();
                let row0 = cpu.dh();
                let col0 = cpu.dl();

                if count == 0 {
                    return;
                }

                let cols = BiosDataArea::read_screen_cols(memory).max(1) as u32;
                let rows = 25u32;
                if u32::from(row0) >= rows || u32::from(col0) >= cols {
                    return;
                }

                let start_linear = u32::from(row0) * cols + u32::from(col0);
                let max_cells = rows.saturating_mul(cols);

                let update_cursor = (mode & 0x01) != 0;
                let attrs_in_string = (mode & 0x02) != 0;

                let src = real_addr(cpu.es(), cpu.bp());

                let mut written: u16 = 0;
                for i in 0..count {
                    let linear = start_linear.saturating_add(u32::from(i));
                    if linear >= max_cells {
                        break;
                    }
                    let row = (linear / cols) as u8;
                    let col = (linear % cols) as u8;

                    let (ch, attr) = if attrs_in_string {
                        let off = u64::from(i).saturating_mul(2);
                        (memory.read_u8(src + off), memory.read_u8(src + off + 1))
                    } else {
                        let off = u64::from(i);
                        (memory.read_u8(src + off), attr_default)
                    };

                    self.video
                        .vga
                        .write_text_cell(memory, page, row, col, ch, attr);
                    written = written.saturating_add(1);
                }

                if update_cursor {
                    let end_linear = start_linear.saturating_add(u32::from(written));
                    let clamped = end_linear.min(max_cells.saturating_sub(1));
                    let row = (clamped / cols) as u8;
                    let col = (clamped % cols) as u8;
                    self.video.vga.set_cursor_pos(memory, page, row, col);
                }
            }
            _ => {
                // Unhandled INT 10h function.
            }
        }
    }
}
