use crate::memory::MemoryBus;

/// BIOS Data Area: current video mode (byte).
pub const BDA_VIDEO_MODE_ADDR: u64 = 0x0449;
/// BIOS Data Area: number of text columns (word).
pub const BDA_SCREEN_COLS_ADDR: u64 = 0x044A;
/// BIOS Data Area: bytes per text page (word).
pub const BDA_PAGE_SIZE_ADDR: u64 = 0x044C;
/// BIOS Data Area: cursor position for page 0 (word; row in high byte, column in low byte).
pub const BDA_CURSOR_POS_PAGE0_ADDR: u64 = 0x0450;
/// BIOS Data Area: cursor shape (word; start scanline in high byte, end scanline in low byte).
pub const BDA_CURSOR_SHAPE_ADDR: u64 = 0x0460;
/// BIOS Data Area: active page number (byte).
pub const BDA_ACTIVE_PAGE_ADDR: u64 = 0x0462;

pub struct BiosDataArea;

impl BiosDataArea {
    pub fn read_video_mode(mem: &mut impl MemoryBus) -> u8 {
        mem.read_u8(BDA_VIDEO_MODE_ADDR)
    }

    pub fn write_video_mode(mem: &mut impl MemoryBus, mode: u8) {
        mem.write_u8(BDA_VIDEO_MODE_ADDR, mode);
    }

    pub fn read_screen_cols(mem: &mut impl MemoryBus) -> u16 {
        mem.read_u16(BDA_SCREEN_COLS_ADDR)
    }

    pub fn write_screen_cols(mem: &mut impl MemoryBus, cols: u16) {
        mem.write_u16(BDA_SCREEN_COLS_ADDR, cols);
    }

    pub fn read_page_size(mem: &mut impl MemoryBus) -> u16 {
        mem.read_u16(BDA_PAGE_SIZE_ADDR)
    }

    pub fn write_page_size(mem: &mut impl MemoryBus, bytes: u16) {
        mem.write_u16(BDA_PAGE_SIZE_ADDR, bytes);
    }

    pub fn read_active_page(mem: &mut impl MemoryBus) -> u8 {
        mem.read_u8(BDA_ACTIVE_PAGE_ADDR)
    }

    pub fn write_active_page(mem: &mut impl MemoryBus, page: u8) {
        mem.write_u8(BDA_ACTIVE_PAGE_ADDR, page);
    }

    pub fn read_cursor_pos(mem: &mut impl MemoryBus, page: u8) -> (u8, u8) {
        if page >= 8 {
            return (0, 0);
        }
        let addr = BDA_CURSOR_POS_PAGE0_ADDR + u64::from(page) * 2;
        let word = mem.read_u16(addr);
        let col = (word & 0xFF) as u8;
        let row = (word >> 8) as u8;
        (row, col)
    }

    pub fn write_cursor_pos(mem: &mut impl MemoryBus, page: u8, row: u8, col: u8) {
        if page >= 8 {
            return;
        }
        let addr = BDA_CURSOR_POS_PAGE0_ADDR + u64::from(page) * 2;
        mem.write_u16(addr, ((row as u16) << 8) | (col as u16));
    }

    pub fn read_cursor_pos_page0(mem: &mut impl MemoryBus) -> (u8, u8) {
        Self::read_cursor_pos(mem, 0)
    }

    pub fn write_cursor_pos_page0(mem: &mut impl MemoryBus, row: u8, col: u8) {
        Self::write_cursor_pos(mem, 0, row, col);
    }

    pub fn read_cursor_shape(mem: &mut impl MemoryBus) -> (u8, u8) {
        let word = mem.read_u16(BDA_CURSOR_SHAPE_ADDR);
        let end = (word & 0xFF) as u8;
        let start = (word >> 8) as u8;
        (start, end)
    }

    pub fn write_cursor_shape(mem: &mut impl MemoryBus, start: u8, end: u8) {
        mem.write_u16(BDA_CURSOR_SHAPE_ADDR, ((start as u16) << 8) | (end as u16));
    }
}
