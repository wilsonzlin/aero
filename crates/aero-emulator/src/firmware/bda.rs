use crate::memory::MemoryBus;

pub const BDA_BASE: u64 = 0x0400;

const VIDEO_MODE_OFFSET: u64 = 0x49;
const TEXT_COLUMNS_OFFSET: u64 = 0x4A;
const VIDEO_PAGE_SIZE_OFFSET: u64 = 0x4C;
const VIDEO_PAGE_OFFSET_OFFSET: u64 = 0x4E;
const ACTIVE_PAGE_OFFSET: u64 = 0x62;
const CURSOR_POS_OFFSET: u64 = 0x50;
const CURSOR_SHAPE_OFFSET: u64 = 0x60;
const ROWS_MINUS_ONE_OFFSET: u64 = 0x84;

pub struct BiosDataArea;

impl BiosDataArea {
    pub fn video_mode(mem: &impl MemoryBus) -> u8 {
        mem.read_u8(BDA_BASE + VIDEO_MODE_OFFSET)
    }

    pub fn set_video_mode(mem: &mut impl MemoryBus, mode: u8) {
        mem.write_u8(BDA_BASE + VIDEO_MODE_OFFSET, mode);
    }

    pub fn text_columns(mem: &impl MemoryBus) -> u16 {
        mem.read_u16(BDA_BASE + TEXT_COLUMNS_OFFSET)
    }

    pub fn set_text_columns(mem: &mut impl MemoryBus, cols: u16) {
        mem.write_u16(BDA_BASE + TEXT_COLUMNS_OFFSET, cols);
    }

    pub fn video_page_size(mem: &impl MemoryBus) -> u16 {
        mem.read_u16(BDA_BASE + VIDEO_PAGE_SIZE_OFFSET)
    }

    pub fn set_video_page_size(mem: &mut impl MemoryBus, bytes: u16) {
        mem.write_u16(BDA_BASE + VIDEO_PAGE_SIZE_OFFSET, bytes);
    }

    pub fn video_page_offset(mem: &impl MemoryBus) -> u16 {
        mem.read_u16(BDA_BASE + VIDEO_PAGE_OFFSET_OFFSET)
    }

    pub fn set_video_page_offset(mem: &mut impl MemoryBus, offset: u16) {
        mem.write_u16(BDA_BASE + VIDEO_PAGE_OFFSET_OFFSET, offset);
    }

    pub fn active_page(mem: &impl MemoryBus) -> u8 {
        mem.read_u8(BDA_BASE + ACTIVE_PAGE_OFFSET)
    }

    pub fn set_active_page(mem: &mut impl MemoryBus, page: u8) {
        mem.write_u8(BDA_BASE + ACTIVE_PAGE_OFFSET, page);
    }

    pub fn cursor_pos(mem: &impl MemoryBus, page: u8) -> (u8, u8) {
        let page = page & 0x07;
        let word = mem.read_u16(BDA_BASE + CURSOR_POS_OFFSET + (page as u64) * 2);
        ((word >> 8) as u8, word as u8)
    }

    pub fn set_cursor_pos(mem: &mut impl MemoryBus, page: u8, row: u8, col: u8) {
        let page = page & 0x07;
        let word = ((row as u16) << 8) | col as u16;
        mem.write_u16(BDA_BASE + CURSOR_POS_OFFSET + (page as u64) * 2, word);
    }

    pub fn cursor_shape(mem: &impl MemoryBus) -> (u8, u8) {
        let word = mem.read_u16(BDA_BASE + CURSOR_SHAPE_OFFSET);
        ((word >> 8) as u8, word as u8)
    }

    pub fn set_cursor_shape(mem: &mut impl MemoryBus, start: u8, end: u8) {
        mem.write_u16(
            BDA_BASE + CURSOR_SHAPE_OFFSET,
            ((start as u16) << 8) | end as u16,
        );
    }

    pub fn text_rows(mem: &impl MemoryBus) -> u8 {
        mem.read_u8(BDA_BASE + ROWS_MINUS_ONE_OFFSET)
            .wrapping_add(1)
    }

    pub fn set_text_rows(mem: &mut impl MemoryBus, rows: u8) {
        mem.write_u8(BDA_BASE + ROWS_MINUS_ONE_OFFSET, rows.saturating_sub(1));
    }
}
