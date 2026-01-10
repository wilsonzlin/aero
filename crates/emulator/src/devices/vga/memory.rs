use std::array;

use super::ports::VgaDevice;
use super::{PLANE_SIZE, VRAM_BASE};

const PAGE_SIZE: usize = 4096;
const VGA_NUM_PLANES: usize = 4;

/// Size of a single VGA plane (64 KiB).
///
/// This is intentionally the same as [`super::PLANE_SIZE`], but is re-exported from
/// `devices::vga` as a convenience for tests/consumers that care about the planar view.
pub const VGA_PLANE_SIZE: usize = PLANE_SIZE;

#[inline]
fn linear_dirty_mask_all_pages() -> u64 {
    let pages = VGA_PLANE_SIZE / PAGE_SIZE;
    match pages {
        0 => 0,
        n if n >= 64 => u64::MAX,
        n => (1u64 << n) - 1,
    }
}

#[inline]
fn plane_dirty_mask_all_pages() -> u16 {
    let pages = VGA_PLANE_SIZE / PAGE_SIZE;
    match pages {
        0 => 0,
        n if n >= 16 => u16::MAX,
        n => (1u16 << n) - 1,
    }
}

/// A typed plane index (0-3).
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct VramPlane(pub usize);

impl VramPlane {
    #[inline]
    pub fn mask(self) -> u8 {
        1u8 << self.0
    }
}

/// VGA VRAM storage + latch pipeline state.
///
/// We keep a canonical planar backing store (4 planes × 64 KiB) for classic VGA behaviour
/// (set/reset, bitmask, write modes, latches, etc). For performance in chain-4 packed modes
/// (e.g. BIOS mode 13h), we also maintain a 64 KiB linear cache that reflects the CPU-visible
/// chain-4 view.
#[derive(Debug, Clone)]
pub struct VgaMemory {
    // ---------------------------------------------------------------------
    // Canonical planar backing store (256 KiB)
    // ---------------------------------------------------------------------
    planes: [Box<[u8; VGA_PLANE_SIZE]>; VGA_NUM_PLANES],
    /// VGA latches, loaded on VRAM reads and used by various write modes.
    latches: [u8; VGA_NUM_PLANES],
    /// Dirty pages per plane, 4 KiB granularity (16 pages per plane).
    dirty_plane_pages: [u16; VGA_NUM_PLANES],

    // ---------------------------------------------------------------------
    // Packed/linear view for chain-4 modes (Mode 13h)
    // ---------------------------------------------------------------------
    linear: Vec<u8>,
    /// Dirty pages in the linear cache, 4 KiB granularity.
    dirty_page_mask: u64,
}

impl Default for VgaMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl VgaMemory {
    pub fn new() -> Self {
        let plane_dirty = plane_dirty_mask_all_pages();
        let linear_dirty = linear_dirty_mask_all_pages();

        Self {
            planes: array::from_fn(|_| Box::new([0u8; VGA_PLANE_SIZE])),
            latches: [0; VGA_NUM_PLANES],
            dirty_plane_pages: [plane_dirty; VGA_NUM_PLANES],
            linear: vec![0u8; VGA_PLANE_SIZE],
            dirty_page_mask: linear_dirty,
        }
    }

    // ---------------------------------------------------------------------
    // Mode 13h / packed helpers (used by renderer tests and simple callers)
    // ---------------------------------------------------------------------

    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.linear
    }

    pub fn write(&mut self, offset: usize, data: &[u8]) {
        if offset >= self.linear.len() || data.is_empty() {
            return;
        }

        let write_len = data.len().min(self.linear.len() - offset);
        self.linear[offset..offset + write_len].copy_from_slice(&data[..write_len]);
        self.mark_dirty_linear_range(offset, write_len);

        // Keep the planar view in sync with chain-4 linear writes.
        for i in 0..write_len {
            let linear = offset + i;
            let plane = VramPlane(linear & 3);
            let plane_off = linear >> 2;
            self.planes[plane.0][plane_off] = self.linear[linear];
            self.mark_dirty_plane(plane, plane_off);
        }
    }

    #[inline]
    pub fn mark_all_dirty(&mut self) {
        let plane_dirty = plane_dirty_mask_all_pages();
        self.dirty_page_mask = linear_dirty_mask_all_pages();
        self.dirty_plane_pages = [plane_dirty; VGA_NUM_PLANES];
    }

    /// Returns and clears the dirty-page bitmask for the linear cache.
    #[inline]
    pub fn take_dirty_pages(&mut self) -> u64 {
        let pages = self.dirty_page_mask;
        self.dirty_page_mask = 0;
        pages
    }

    /// Returns and clears the dirty-page bitmask for each plane.
    pub fn take_dirty_plane_pages(&mut self) -> [u16; VGA_NUM_PLANES] {
        let dirty = self.dirty_plane_pages;
        self.dirty_plane_pages = [0; VGA_NUM_PLANES];
        dirty
    }

    #[inline]
    fn mark_dirty_linear_range(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }

        let start_page = offset / PAGE_SIZE;
        let end_page = (offset + len - 1) / PAGE_SIZE;
        for page in start_page..=end_page {
            if page >= 64 {
                break;
            }
            self.dirty_page_mask |= 1u64 << page;
        }
    }

    #[inline]
    fn mark_dirty_linear(&mut self, linear_offset: usize) {
        let page = linear_offset / PAGE_SIZE;
        if page < 64 {
            self.dirty_page_mask |= 1u64 << page;
        }
    }

    // ---------------------------------------------------------------------
    // Planar view accessors (useful for planar renderers/tests)
    // ---------------------------------------------------------------------

    #[inline]
    pub fn plane(&self, plane: VramPlane) -> &[u8; VGA_PLANE_SIZE] {
        &self.planes[plane.0]
    }

    #[inline]
    pub fn plane_mut(&mut self, plane: VramPlane) -> &mut [u8; VGA_PLANE_SIZE] {
        &mut self.planes[plane.0]
    }

    /// Direct plane write (used by tests and debugging).
    pub fn write_plane_byte(&mut self, plane: usize, offset: usize, value: u8) {
        assert!(plane < VGA_NUM_PLANES);
        assert!(offset < VGA_PLANE_SIZE);
        self.planes[plane][offset] = value;
        self.mark_dirty_plane(VramPlane(plane), offset);
    }

    // ---------------------------------------------------------------------
    // Legacy planar helpers (still used by existing Mode 12h tests)
    // ---------------------------------------------------------------------

    /// CPU-side read using the active VGA memory map select + addressing mode.
    ///
    /// This is a convenience wrapper around [`Self::read_u8`] that accepts a [`VgaDevice`].
    pub fn read_u8_planar(&mut self, regs: &VgaDevice, addr: u32) -> u8 {
        self.read_u8(addr as u64, &regs.seq_regs, &regs.gc_regs)
            .unwrap_or(0)
    }

    /// CPU-side write using the active VGA memory map select + addressing mode.
    ///
    /// This is a convenience wrapper around [`Self::write_u8`] that accepts a [`VgaDevice`].
    pub fn write_u8_planar(&mut self, regs: &VgaDevice, addr: u32, value: u8) {
        let _ = self.write_u8(addr as u64, value, &regs.seq_regs, &regs.gc_regs);
    }

    // ---------------------------------------------------------------------
    // VGA latch/read/write pipeline (CPU-visible VRAM access)
    // ---------------------------------------------------------------------

    /// Read a byte from VGA VRAM, returning `None` if the address is not mapped into the active
    /// VGA aperture (as selected by GC reg 0x06).
    pub fn read_u8(&mut self, phys_addr: u64, seq_regs: &[u8], gc_regs: &[u8]) -> Option<u8> {
        let decoded = decode_address(phys_addr, seq_regs, gc_regs)?;
        self.load_latches(decoded.offset);

        let gc_mode = gc_regs.get(5).copied().unwrap_or(0);
        if gc_read_mode(gc_mode) == 0 {
            let read_map_select = gc_regs.get(4).copied().unwrap_or(0);
            let plane = decoded
                .address_plane
                .unwrap_or(VramPlane((read_map_select & 0x03) as usize));
            Some(self.latches[plane.0])
        } else {
            let color_compare = gc_regs.get(2).copied().unwrap_or(0);
            let color_dont_care = gc_regs.get(7).copied().unwrap_or(0);
            Some(self.read_mode_1_color_compare(color_compare, color_dont_care))
        }
    }

    /// Write a byte to VGA VRAM, returning whether the write hit the active VGA aperture.
    pub fn write_u8(&mut self, phys_addr: u64, value: u8, seq_regs: &[u8], gc_regs: &[u8]) -> bool {
        let decoded = match decode_address(phys_addr, seq_regs, gc_regs) {
            Some(decoded) => decoded,
            None => return false,
        };

        let seq_map_mask = seq_regs.get(2).copied().unwrap_or(0) & 0x0f;
        let gc_mode = gc_regs.get(5).copied().unwrap_or(0);
        let write_mode = gc_write_mode(gc_mode);

        let gc_data_rotate = gc_regs.get(3).copied().unwrap_or(0);
        let gc_enable_set_reset = gc_regs.get(1).copied().unwrap_or(0) & 0x0f;
        let gc_bit_mask = gc_regs.get(8).copied().unwrap_or(0);

        // Hot path: chain-4 packed writes (Mode 13h) are overwhelmingly common.
        //
        // When the VGA pipeline is configured as a no-op (write mode 0, no logical op, full
        // bitmask, no set/reset), we can just store the byte directly without touching latches.
        if decoded.is_chain4
            && write_mode == 0
            && (gc_data_rotate & 0x1f) == 0
            && gc_enable_set_reset == 0
            && gc_bit_mask == 0xff
        {
            let plane = decoded
                .address_plane
                .expect("chain4 decode must always select a plane");
            if (seq_map_mask & plane.mask()) != 0 {
                self.store_plane_byte(plane, decoded.offset, value, decoded.chain4_linear);
            }
            return true;
        }

        // Real VGA does an internal read (to latches) for read-modify-write in write modes 0/2/3.
        // Write mode 1 must *not* reload latches from the destination address; it's specifically
        // used to copy previously-latched data.
        if write_mode != 1 {
            self.load_latches(decoded.offset);
        }

        let plane_enable_mask = seq_map_mask & decoded.address_plane_mask;
        if plane_enable_mask == 0 {
            return true;
        }

        let rotate_count = gc_data_rotate & 0x07;
        let logical_op = (gc_data_rotate >> 3) & 0x03;

        match write_mode {
            0 => {
                let rotated = value.rotate_right(rotate_count as u32);
                let set_reset = gc_regs.get(0).copied().unwrap_or(0) & 0x0f;
                let enable_set_reset = gc_enable_set_reset;
                let bit_mask = gc_bit_mask;

                for plane_idx in 0..VGA_NUM_PLANES {
                    let plane = VramPlane(plane_idx);
                    if (plane_enable_mask & plane.mask()) == 0 {
                        continue;
                    }

                    let mut data = rotated;
                    if (enable_set_reset & plane.mask()) != 0 {
                        data = if (set_reset & plane.mask()) != 0 { 0xff } else { 0x00 };
                    }

                    data = apply_logical_op(logical_op, data, self.latches[plane_idx]);
                    data = (data & bit_mask) | (self.latches[plane_idx] & !bit_mask);

                    self.store_plane_byte(plane, decoded.offset, data, decoded.chain4_linear_for_plane(plane));
                }
            }
            1 => {
                for plane_idx in 0..VGA_NUM_PLANES {
                    let plane = VramPlane(plane_idx);
                    if (plane_enable_mask & plane.mask()) == 0 {
                        continue;
                    }
                    let data = self.latches[plane_idx];
                    self.store_plane_byte(plane, decoded.offset, data, decoded.chain4_linear_for_plane(plane));
                }
            }
            2 => {
                let rotated = value.rotate_right(rotate_count as u32);
                let bit_mask = gc_bit_mask;

                for plane_idx in 0..VGA_NUM_PLANES {
                    let plane = VramPlane(plane_idx);
                    if (plane_enable_mask & plane.mask()) == 0 {
                        continue;
                    }

                    let mut data = if (rotated & plane.mask()) != 0 { 0xff } else { 0x00 };
                    data = apply_logical_op(logical_op, data, self.latches[plane_idx]);
                    data = (data & bit_mask) | (self.latches[plane_idx] & !bit_mask);

                    self.store_plane_byte(plane, decoded.offset, data, decoded.chain4_linear_for_plane(plane));
                }
            }
            3 => {
                let rotated = value.rotate_right(rotate_count as u32);
                let bit_mask = rotated & gc_bit_mask;
                let set_reset = gc_regs.get(0).copied().unwrap_or(0) & 0x0f;

                for plane_idx in 0..VGA_NUM_PLANES {
                    let plane = VramPlane(plane_idx);
                    if (plane_enable_mask & plane.mask()) == 0 {
                        continue;
                    }

                    let mut data = if (set_reset & plane.mask()) != 0 { 0xff } else { 0x00 };
                    data = apply_logical_op(logical_op, data, self.latches[plane_idx]);
                    data = (data & bit_mask) | (self.latches[plane_idx] & !bit_mask);

                    self.store_plane_byte(plane, decoded.offset, data, decoded.chain4_linear_for_plane(plane));
                }
            }
            _ => unreachable!("write_mode must be 0..=3"),
        }

        true
    }

    #[inline]
    fn mark_dirty_plane(&mut self, plane: VramPlane, offset: usize) {
        let page = (offset >> 12) & 0x0f;
        self.dirty_plane_pages[plane.0] |= 1u16 << page;
    }

    #[inline]
    fn store_plane_byte(&mut self, plane: VramPlane, offset: usize, value: u8, chain4_linear: Option<usize>) {
        self.planes[plane.0][offset] = value;
        self.mark_dirty_plane(plane, offset);

        if let Some(linear) = chain4_linear {
            if linear < self.linear.len() {
                self.linear[linear] = value;
                self.mark_dirty_linear(linear);
            }
        }
    }

    #[inline]
    fn load_latches(&mut self, offset: usize) {
        let offset = offset & (VGA_PLANE_SIZE - 1);
        for plane_idx in 0..VGA_NUM_PLANES {
            self.latches[plane_idx] = self.planes[plane_idx][offset];
        }
    }

    #[inline]
    fn read_mode_1_color_compare(&self, color_compare: u8, color_dont_care: u8) -> u8 {
        let mut diff = 0u8;
        let compare = color_compare & 0x0f;
        let dont_care = color_dont_care & 0x0f;

        for plane_idx in 0..VGA_NUM_PLANES {
            let plane_mask = 1u8 << plane_idx;
            // "Color Don't Care" is a *mask* of planes to compare; cleared bits are treated as
            // don't-care.
            let care_mask = if (dont_care & plane_mask) != 0 { 0xff } else { 0x00 };
            let compare_byte = if (compare & plane_mask) != 0 { 0xff } else { 0x00 };
            diff |= (self.latches[plane_idx] ^ compare_byte) & care_mask;
        }

        !diff
    }
}

#[derive(Debug, Copy, Clone)]
struct DecodedAddress {
    /// Address into the per-plane storage.
    offset: usize,
    /// Plane selected by address (chain-4 / odd/even), if any.
    address_plane: Option<VramPlane>,
    /// A 4-bit mask of which planes are addressable at this address.
    address_plane_mask: u8,
    /// If chain-4 is active, the original linear CPU offset within the current window.
    chain4_linear: Option<usize>,
    is_chain4: bool,
}

impl DecodedAddress {
    #[inline]
    fn chain4_linear_for_plane(self, plane: VramPlane) -> Option<usize> {
        if !self.is_chain4 {
            return None;
        }
        if self.address_plane == Some(plane) {
            self.chain4_linear
        } else {
            None
        }
    }
}

#[inline]
fn gc_write_mode(gc_mode: u8) -> u8 {
    gc_mode & 0x03
}

#[inline]
fn gc_read_mode(gc_mode: u8) -> u8 {
    (gc_mode >> 3) & 0x01
}

#[inline]
fn apply_logical_op(op: u8, data: u8, latch: u8) -> u8 {
    match op {
        0 => data,
        1 => data & latch,
        2 => data | latch,
        3 => data ^ latch,
        _ => unreachable!("logical op must be 0..=3"),
    }
}

#[inline]
fn decode_address(phys_addr: u64, seq_regs: &[u8], gc_regs: &[u8]) -> Option<DecodedAddress> {
    let phys_addr_u32 = u32::try_from(phys_addr).ok()?;

    let gc_misc = gc_regs.get(6).copied().unwrap_or(0);
    let (base, size) = match (gc_misc >> 2) & 0x03 {
        0 => (VRAM_BASE, 0x20000u32),
        1 => (VRAM_BASE, 0x10000u32),
        2 => (0xB0000u32, 0x08000u32),
        3 => (0xB8000u32, 0x08000u32),
        _ => unreachable!(),
    };

    if phys_addr_u32 < base || phys_addr_u32 >= base + size {
        return None;
    }

    let mut cpu_offset = phys_addr_u32 - base;

    // Addressing mode select.
    let seq_mem_mode = seq_regs.get(4).copied().unwrap_or(0);
    let chain4 = (seq_mem_mode & 0x08) != 0;
    if chain4 {
        let plane = VramPlane((cpu_offset & 0x03) as usize);
        let linear = cpu_offset as usize;
        cpu_offset >>= 2;
        return Some(DecodedAddress {
            offset: (cpu_offset as usize) & (VGA_PLANE_SIZE - 1),
            address_plane: Some(plane),
            address_plane_mask: plane.mask(),
            chain4_linear: Some(linear),
            is_chain4: true,
        });
    }

    // Odd/even addressing is controlled by SEQ memory mode bit 2 (odd/even disable) and GC mode
    // bit 4 (host odd/even enable).
    let seq_odd_even_disable = (seq_mem_mode & 0x04) != 0;
    let gc_mode = gc_regs.get(5).copied().unwrap_or(0);
    let gc_odd_even_enable = (gc_mode & 0x10) != 0;
    let odd_even = (!seq_odd_even_disable) && gc_odd_even_enable;
    if odd_even {
        let plane = VramPlane((cpu_offset & 0x01) as usize);
        cpu_offset >>= 1;
        return Some(DecodedAddress {
            offset: (cpu_offset as usize) & (VGA_PLANE_SIZE - 1),
            address_plane: Some(plane),
            address_plane_mask: plane.mask(),
            chain4_linear: None,
            is_chain4: false,
        });
    }

    Some(DecodedAddress {
        offset: (cpu_offset as usize) & (VGA_PLANE_SIZE - 1),
        address_plane: None,
        address_plane_mask: 0x0f,
        chain4_linear: None,
        is_chain4: false,
    })
}
