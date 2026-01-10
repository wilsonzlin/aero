use crate::{
    cpu::CpuState, devices::vga::VgaDevice, firmware::bda::BiosDataArea, memory::MemoryBus,
};

const DEFAULT_TEXT_ATTR: u8 = 0x07;

pub fn set_video_mode(cpu: &mut CpuState, mem: &mut impl MemoryBus, vga: &mut VgaDevice) {
    let al = cpu.al();
    let mode = al & 0x7F;
    let clear = al & 0x80 == 0;

    if vga.set_mode(mode, clear).is_err() {
        return;
    }

    let (cols, rows) = vga.text_dimensions();
    BiosDataArea::set_video_mode(mem, mode);
    BiosDataArea::set_text_columns(mem, cols as u16);
    BiosDataArea::set_text_rows(mem, rows);
    BiosDataArea::set_video_page_size(mem, cols as u16 * rows as u16 * 2);
    BiosDataArea::set_video_page_offset(mem, 0);
    BiosDataArea::set_active_page(mem, 0);
    BiosDataArea::set_cursor_shape(mem, 0x06, 0x07);
    for page in 0..8u8 {
        BiosDataArea::set_cursor_pos(mem, page, 0, 0);
    }
}

pub fn get_video_mode(cpu: &mut CpuState, mem: &impl MemoryBus) {
    let mode = BiosDataArea::video_mode(mem);
    let cols = BiosDataArea::text_columns(mem);
    let page = BiosDataArea::active_page(mem);

    cpu.set_al(mode);
    cpu.set_ah(cols as u8);
    cpu.set_bh(page);
}

pub fn set_cursor_shape(cpu: &mut CpuState, mem: &mut impl MemoryBus) {
    BiosDataArea::set_cursor_shape(mem, cpu.ch(), cpu.cl());
}

pub fn set_cursor_position(cpu: &mut CpuState, mem: &mut impl MemoryBus) {
    let page = cpu.bh();
    let row = cpu.dh();
    let col = cpu.dl();
    let rows = BiosDataArea::text_rows(mem);
    let cols = BiosDataArea::text_columns(mem) as u8;

    BiosDataArea::set_cursor_pos(
        mem,
        page,
        row.min(rows.saturating_sub(1)),
        col.min(cols.saturating_sub(1)),
    );
}

pub fn get_cursor_position(cpu: &mut CpuState, mem: &impl MemoryBus) {
    let page = cpu.bh();
    let (row, col) = BiosDataArea::cursor_pos(mem, page);
    let (start, end) = BiosDataArea::cursor_shape(mem);

    cpu.set_dh(row);
    cpu.set_dl(col);
    cpu.set_ch(start);
    cpu.set_cl(end);
}

pub fn tty_output(cpu: &mut CpuState, mem: &mut impl MemoryBus, vga: &mut VgaDevice) {
    let page = cpu.bh();
    let ch = cpu.al();
    let mut attr = cpu.bl();
    if attr == 0 {
        attr = DEFAULT_TEXT_ATTR;
    }

    let cols = BiosDataArea::text_columns(mem).max(1) as u8;
    let rows = BiosDataArea::text_rows(mem).max(1);
    let (mut row, mut col) = BiosDataArea::cursor_pos(mem, page);

    match ch {
        0x07 => {}
        0x08 => {
            if col > 0 {
                col -= 1;
            } else if row > 0 {
                row -= 1;
                col = cols.saturating_sub(1);
            }
        }
        0x0D => {
            col = 0;
        }
        0x0A => {
            row = row.saturating_add(1);
        }
        _ => {
            vga.write_text_cell(row, col, ch, attr);
            col = col.saturating_add(1);
            if col >= cols {
                col = 0;
                row = row.saturating_add(1);
            }
        }
    }

    if row >= rows {
        vga.scroll_text_window_up(0, 0, rows - 1, cols - 1, 1, attr);
        row = rows - 1;
    }

    BiosDataArea::set_cursor_pos(mem, page, row, col);
}

pub fn scroll_up(cpu: &mut CpuState, mem: &impl MemoryBus, vga: &mut VgaDevice) {
    let lines = cpu.al();
    let blank_attr = cpu.bh();
    let top = cpu.ch();
    let left = cpu.cl();
    let bottom = cpu.dh();
    let right = cpu.dl();

    let cols = BiosDataArea::text_columns(mem).max(1) as u8;
    let rows = BiosDataArea::text_rows(mem).max(1);

    let top = top.min(rows - 1);
    let bottom = bottom.min(rows - 1);
    let left = left.min(cols - 1);
    let right = right.min(cols - 1);

    vga.scroll_text_window_up(top, left, bottom, right, lines, blank_attr);
}

pub fn write_char_attr(cpu: &mut CpuState, mem: &impl MemoryBus, vga: &mut VgaDevice) {
    let page = cpu.bh();
    let ch = cpu.al();
    let attr = cpu.bl();
    let count = cpu.cx();
    if count == 0 {
        return;
    }

    let cols = BiosDataArea::text_columns(mem).max(1) as u8;
    let rows = BiosDataArea::text_rows(mem).max(1);
    let (row0, col0) = BiosDataArea::cursor_pos(mem, page);

    let mut linear = row0 as u32 * cols as u32 + col0 as u32;
    let max = rows as u32 * cols as u32;

    for _ in 0..count {
        if linear >= max {
            break;
        }
        let row = (linear / cols as u32) as u8;
        let col = (linear % cols as u32) as u8;
        vga.write_text_cell(row, col, ch, attr);
        linear += 1;
    }
}

pub fn write_char_only(cpu: &mut CpuState, mem: &impl MemoryBus, vga: &mut VgaDevice) {
    let page = cpu.bh();
    let ch = cpu.al();
    let count = cpu.cx();
    if count == 0 {
        return;
    }

    let cols = BiosDataArea::text_columns(mem).max(1) as u8;
    let rows = BiosDataArea::text_rows(mem).max(1);
    let (row0, col0) = BiosDataArea::cursor_pos(mem, page);

    let mut linear = row0 as u32 * cols as u32 + col0 as u32;
    let max = rows as u32 * cols as u32;

    for _ in 0..count {
        if linear >= max {
            break;
        }
        let row = (linear / cols as u32) as u8;
        let col = (linear % cols as u32) as u8;
        let (_, attr) = vga.read_text_cell(row, col);
        vga.write_text_cell(row, col, ch, attr);
        linear += 1;
    }
}

pub fn write_string(cpu: &mut CpuState, mem: &mut impl MemoryBus, vga: &mut VgaDevice) {
    let write_mode = cpu.al();
    let page = cpu.bh();
    let attr = cpu.bl();
    let len = cpu.cx() as usize;
    let mut row = cpu.dh();
    let mut col = cpu.dl();

    let cols = BiosDataArea::text_columns(mem).max(1) as u8;
    let rows = BiosDataArea::text_rows(mem).max(1);

    let mut addr = cpu.es.base() + (cpu.rbp & 0xFFFF);
    let mut written = 0usize;

    while written < len {
        if row >= rows {
            break;
        }

        let ch = mem.read_u8(addr);
        addr += 1;

        let cell_attr = if write_mode & 0x02 != 0 {
            let a = mem.read_u8(addr);
            addr += 1;
            a
        } else {
            attr
        };

        vga.write_text_cell(row, col, ch, cell_attr);
        written += 1;

        col = col.saturating_add(1);
        if col >= cols {
            col = 0;
            row = row.saturating_add(1);
        }
    }

    if write_mode & 0x01 != 0 {
        BiosDataArea::set_cursor_pos(mem, page, row, col);
    }
}
