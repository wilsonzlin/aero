use crate::devices::vga::{PLANE_SIZE, VRAM_BASE, VRAM_SIZE};

use super::ports::VgaDevice;

const PAGE_SIZE: usize = 4096;
const VRAM_TOTAL_PAGES: usize = VRAM_SIZE / PAGE_SIZE;

#[derive(Debug)]
pub struct VgaMemory {
    vram: Vec<u8>,
    latches: [u8; 4],
    dirty_page_mask: u64,
}

impl VgaMemory {
    pub fn new() -> Self {
        Self {
            vram: vec![0u8; VRAM_SIZE],
            latches: [0u8; 4],
            dirty_page_mask: u64::MAX,
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
        self.dirty_page_mask = u64::MAX;
    }

    #[inline]
    fn mark_dirty_range(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }

        let start_page = offset / PAGE_SIZE;
        let end_page = (offset + len - 1) / PAGE_SIZE;
        for page in start_page..=end_page {
            if page >= VRAM_TOTAL_PAGES {
                break;
            }
            self.dirty_page_mask |= 1u64 << page;
        }
    }

    /// Returns and clears the bitmask of dirty pages.
    #[inline]
    pub fn take_dirty_pages(&mut self) -> u64 {
        let pages = self.dirty_page_mask;
        self.dirty_page_mask = 0;
        pages
    }

    /// Direct plane write (used for tests and debugging).
    pub fn write_plane_byte(&mut self, plane: usize, offset: usize, value: u8) {
        assert!(plane < 4);
        assert!(offset < PLANE_SIZE);
        let addr = plane * PLANE_SIZE + offset;
        self.vram[addr] = value;
        self.mark_dirty_range(addr, 1);
    }

    fn load_latches(&mut self, offset: usize) {
        for plane in 0..4 {
            self.latches[plane] = self.vram[plane * PLANE_SIZE + offset];
        }
    }

    /// CPU-side read from VGA planar memory window (A000:0000).
    ///
    /// This updates VGA latches and returns the byte from the selected read plane.
    pub fn read_u8_planar(&mut self, regs: &VgaDevice, addr: u32) -> u8 {
        if !(VRAM_BASE..VRAM_BASE + PLANE_SIZE as u32).contains(&addr) {
            return 0;
        }
        let offset = (addr - VRAM_BASE) as usize;
        self.load_latches(offset);
        let plane = (regs.gc_regs.get(4).copied().unwrap_or(0) & 0x03) as usize;
        self.latches[plane]
    }

    /// CPU-side write into VGA planar memory window (A000:0000), implementing
    /// the VGA/EGA planar write modes (0..=3) with Set/Reset + Bit Mask.
    pub fn write_u8_planar(&mut self, regs: &VgaDevice, addr: u32, value: u8) {
        if !(VRAM_BASE..VRAM_BASE + PLANE_SIZE as u32).contains(&addr) {
            return;
        }
        let offset = (addr - VRAM_BASE) as usize;

        let write_mode = regs.gc_regs.get(5).copied().unwrap_or(0) & 0x03;

        if write_mode != 1 {
            // For write modes that depend on existing memory contents, emulate the VGA's
            // read-modify-write behavior by refreshing latches from the destination byte.
            self.load_latches(offset);
        }

        let data_rotate = regs.gc_regs.get(3).copied().unwrap_or(0);
        let rotate_count = data_rotate & 0x07;
        let func_select = (data_rotate >> 3) & 0x03;
        let bit_mask = regs.gc_regs.get(8).copied().unwrap_or(0xFF);

        let rotated = value.rotate_right(rotate_count as u32);

        let map_mask = regs.seq_regs.get(2).copied().unwrap_or(0);
        let set_reset = regs.gc_regs.get(0).copied().unwrap_or(0);
        let enable_set_reset = regs.gc_regs.get(1).copied().unwrap_or(0);

        for plane in 0..4 {
            let plane_mask_bit = 1u8 << plane;
            if map_mask & plane_mask_bit == 0 {
                continue;
            }

            let latch = self.latches[plane];
            let result = match write_mode {
                0 => {
                    let mut data = rotated;
                    if (enable_set_reset & plane_mask_bit) != 0 {
                        data = if (set_reset & plane_mask_bit) != 0 { 0xFF } else { 0x00 };
                    }

                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };

                    (alu & bit_mask) | (latch & !bit_mask)
                }
                1 => latch,
                2 => {
                    let data = if (value & plane_mask_bit) != 0 { 0xFF } else { 0x00 };
                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };
                    (alu & bit_mask) | (latch & !bit_mask)
                }
                3 => {
                    let data = if (set_reset & plane_mask_bit) != 0 { 0xFF } else { 0x00 };
                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };
                    let mask = bit_mask & rotated;
                    (alu & mask) | (latch & !mask)
                }
                _ => unreachable!("VGA write mode {write_mode} is invalid"),
            };

            let addr = plane * PLANE_SIZE + offset;
            self.vram[addr] = result;
            self.mark_dirty_range(addr, 1);
        }
    }
}

