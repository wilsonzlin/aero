use core::ops::Range;

const DIRTY_PAGE_SIZE: usize = 4096;

#[derive(Debug, Clone)]
struct DirtyBitmap {
    bits: Vec<u64>,
    pages: usize,
    page_size: usize,
}

impl DirtyBitmap {
    fn new(mem_len: usize, page_size: usize) -> Self {
        let pages = mem_len.div_ceil(page_size);
        let words = pages.div_ceil(64);
        Self {
            bits: vec![0; words],
            pages,
            page_size,
        }
    }

    fn mark_addr(&mut self, addr: usize) {
        let page = addr / self.page_size;
        if page < self.pages {
            let word = page / 64;
            let bit = page % 64;
            self.bits[word] |= 1u64 << bit;
        }
    }

    fn take(&mut self) -> Vec<u64> {
        let mut pages = Vec::new();
        for (word_idx, word) in self.bits.iter_mut().enumerate() {
            let mut w = *word;
            if w == 0 {
                continue;
            }
            *word = 0;
            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                let page = word_idx * 64 + bit;
                if page < self.pages {
                    pages.push(page as u64);
                }
                w &= !(1u64 << bit);
            }
        }
        pages
    }

    fn clear(&mut self) {
        self.bits.fill(0);
    }
}

/// CPU-visible memory access trait (physical addressing).
///
/// This mirrors the interface contract in `docs/15-agent-task-breakdown.md`.
pub trait MemoryAccess {
    fn read_u8(&self, addr: u64) -> u8;

    fn read_u16(&self, addr: u64) -> u16 {
        u16::from_le_bytes([self.read_u8(addr), self.read_u8(addr + 1)])
    }

    fn read_u32(&self, addr: u64) -> u32 {
        u32::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr + 1),
            self.read_u8(addr + 2),
            self.read_u8(addr + 3),
        ])
    }

    fn read_u64(&self, addr: u64) -> u64 {
        u64::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr + 1),
            self.read_u8(addr + 2),
            self.read_u8(addr + 3),
            self.read_u8(addr + 4),
            self.read_u8(addr + 5),
            self.read_u8(addr + 6),
            self.read_u8(addr + 7),
        ])
    }

    fn read_u128(&self, addr: u64) -> u128 {
        u128::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr + 1),
            self.read_u8(addr + 2),
            self.read_u8(addr + 3),
            self.read_u8(addr + 4),
            self.read_u8(addr + 5),
            self.read_u8(addr + 6),
            self.read_u8(addr + 7),
            self.read_u8(addr + 8),
            self.read_u8(addr + 9),
            self.read_u8(addr + 10),
            self.read_u8(addr + 11),
            self.read_u8(addr + 12),
            self.read_u8(addr + 13),
            self.read_u8(addr + 14),
            self.read_u8(addr + 15),
        ])
    }

    fn write_u8(&mut self, addr: u64, val: u8);

    fn write_u16(&mut self, addr: u64, val: u16) {
        let [b0, b1] = val.to_le_bytes();
        self.write_u8(addr, b0);
        self.write_u8(addr + 1, b1);
    }

    fn write_u32(&mut self, addr: u64, val: u32) {
        let [b0, b1, b2, b3] = val.to_le_bytes();
        self.write_u8(addr, b0);
        self.write_u8(addr + 1, b1);
        self.write_u8(addr + 2, b2);
        self.write_u8(addr + 3, b3);
    }

    fn write_u64(&mut self, addr: u64, val: u64) {
        let bytes = val.to_le_bytes();
        for (i, b) in bytes.iter().enumerate() {
            self.write_u8(addr + i as u64, *b);
        }
    }

    fn write_u128(&mut self, addr: u64, val: u128) {
        let bytes = val.to_le_bytes();
        for (i, b) in bytes.iter().enumerate() {
            self.write_u8(addr + i as u64, *b);
        }
    }

    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read_u8(addr + i as u64);
        }
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        for (i, b) in buf.iter().enumerate() {
            self.write_u8(addr + i as u64, *b);
        }
    }

    fn fetch_code(&self, addr: u64, len: usize) -> &[u8];
}

/// Firmware-only memory operations (mapping ROM).
pub trait FirmwareMemory {
    /// Map a ROM blob at `base` and make it read-only for guest writes.
    fn map_rom(&mut self, base: u64, rom: &[u8]);
}

/// A20 gate controller.
///
/// Real x86 systems historically used the i8042 controller; modern chipsets
/// provide a "fast A20" latch at I/O port 0x92. The firmware should call into
/// this trait so A20 gating can live in the chipset/memory bus.
pub trait A20Gate {
    fn set_a20_enabled(&mut self, enabled: bool);
    fn a20_enabled(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct PhysicalMemory {
    data: Vec<u8>,
    a20_enabled: bool,
    read_only_ranges: Vec<Range<u64>>,
    dirty: DirtyBitmap,
}

impl PhysicalMemory {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            a20_enabled: false,
            read_only_ranges: Vec::new(),
            dirty: DirtyBitmap::new(size, DIRTY_PAGE_SIZE),
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Read bytes directly from the backing store (no A20 translation).
    pub fn read_raw(&self, addr: u64, buf: &mut [u8]) {
        let start: usize = addr
            .try_into()
            .unwrap_or_else(|_| panic!("address out of range: 0x{addr:016x}"));
        let end = start
            .checked_add(buf.len())
            .unwrap_or_else(|| panic!("address overflow: 0x{addr:016x}+0x{:x}", buf.len()));
        assert!(
            end <= self.data.len(),
            "raw read out of bounds: 0x{addr:016x}+0x{:x} (mem=0x{:x})",
            buf.len(),
            self.data.len()
        );
        buf.copy_from_slice(&self.data[start..end]);
    }

    /// Write bytes directly into the backing store (no A20 translation, ignores read-only ranges).
    pub fn write_raw(&mut self, addr: u64, buf: &[u8]) {
        let start: usize = addr
            .try_into()
            .unwrap_or_else(|_| panic!("address out of range: 0x{addr:016x}"));
        let end = start
            .checked_add(buf.len())
            .unwrap_or_else(|| panic!("address overflow: 0x{addr:016x}+0x{:x}", buf.len()));
        assert!(
            end <= self.data.len(),
            "raw write out of bounds: 0x{addr:016x}+0x{:x} (mem=0x{:x})",
            buf.len(),
            self.data.len()
        );
        self.data[start..end].copy_from_slice(buf);
    }

    pub fn read_only_ranges(&self) -> &[Range<u64>] {
        &self.read_only_ranges
    }

    pub fn set_read_only_ranges(&mut self, ranges: Vec<Range<u64>>) {
        self.read_only_ranges = ranges;
    }

    pub fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.dirty.take()
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    fn translate_addr(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & 0x000F_FFFF
        }
    }

    fn to_index(&self, addr: u64) -> usize {
        let addr = self.translate_addr(addr);
        addr.try_into()
            .unwrap_or_else(|_| panic!("address out of range: 0x{addr:016x}"))
    }

    fn is_read_only(&self, addr: u64) -> bool {
        let addr = self.translate_addr(addr);
        self.read_only_ranges
            .iter()
            .any(|r| addr >= r.start && addr < r.end)
    }

    pub fn read_bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        self.read_physical(addr, &mut buf);
        buf
    }
}

impl A20Gate for PhysicalMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

impl FirmwareMemory for PhysicalMemory {
    fn map_rom(&mut self, base: u64, rom: &[u8]) {
        let base_usize = self.to_index(base);
        let end = base_usize
            .checked_add(rom.len())
            .unwrap_or_else(|| panic!("ROM mapping overflow"));
        assert!(
            end <= self.data.len(),
            "ROM mapping out of bounds: 0x{base:016x}+0x{:x} (mem=0x{:x})",
            rom.len(),
            self.data.len()
        );
        self.data[base_usize..end].copy_from_slice(rom);
        self.read_only_ranges.push(base..base + rom.len() as u64);
    }
}

impl MemoryAccess for PhysicalMemory {
    fn read_u8(&self, addr: u64) -> u8 {
        let idx = self.to_index(addr);
        self.data[idx]
    }

    fn write_u8(&mut self, addr: u64, val: u8) {
        if self.is_read_only(addr) {
            return;
        }
        let idx = self.to_index(addr);
        self.data[idx] = val;
        self.dirty.mark_addr(idx);
    }

    fn fetch_code(&self, addr: u64, len: usize) -> &[u8] {
        let idx = self.to_index(addr);
        let end = idx + len;
        &self.data[idx..end]
    }
}
