use core::ops::Range;

/// Abstract memory bus used by the interpreter.
///
/// The real project will have paging/MMIO. For now we model linear memory and
/// expose optional bulk operations used by REP-prefixed string instructions.
pub trait Bus {
    fn read_u8(&mut self, addr: u64) -> u8;
    fn write_u8(&mut self, addr: u64, value: u8);

    fn read_u16(&mut self, addr: u64) -> u16 {
        let lo = self.read_u8(addr) as u16;
        let hi = self.read_u8(addr + 1) as u16;
        lo | (hi << 8)
    }

    fn read_u32(&mut self, addr: u64) -> u32 {
        let b0 = self.read_u8(addr) as u32;
        let b1 = self.read_u8(addr + 1) as u32;
        let b2 = self.read_u8(addr + 2) as u32;
        let b3 = self.read_u8(addr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn read_u64(&mut self, addr: u64) -> u64 {
        let lo = self.read_u32(addr) as u64;
        let hi = self.read_u32(addr + 4) as u64;
        lo | (hi << 32)
    }

    fn read_u128(&mut self, addr: u64) -> u128 {
        let lo = self.read_u64(addr) as u128;
        let hi = self.read_u64(addr + 8) as u128;
        lo | (hi << 64)
    }

    fn write_u16(&mut self, addr: u64, value: u16) {
        self.write_u8(addr, (value & 0x00FF) as u8);
        self.write_u8(addr + 1, (value >> 8) as u8);
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        self.write_u8(addr, (value & 0x0000_00FF) as u8);
        self.write_u8(addr + 1, ((value >> 8) & 0x0000_00FF) as u8);
        self.write_u8(addr + 2, ((value >> 16) & 0x0000_00FF) as u8);
        self.write_u8(addr + 3, ((value >> 24) & 0x0000_00FF) as u8);
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        self.write_u32(addr, (value & 0xFFFF_FFFF) as u32);
        self.write_u32(addr + 4, (value >> 32) as u32);
    }

    fn write_u128(&mut self, addr: u64, value: u128) {
        self.write_u64(addr, value as u64);
        self.write_u64(addr + 8, (value >> 64) as u64);
    }

    /// Whether the bus can perform fast, contiguous copies between RAM regions.
    ///
    /// The interpreter uses this as a hint for REP MOVS* fast paths.
    fn supports_bulk_copy(&self) -> bool {
        false
    }

    /// Fast copy of `len` bytes from `src` to `dst`.
    ///
    /// Returns `true` if the copy was performed; `false` if unsupported.
    ///
    /// Implementations may assume `src..src+len` and `dst..dst+len` are valid.
    fn bulk_copy(&mut self, _dst: u64, _src: u64, _len: usize) -> bool {
        false
    }

    /// Whether the bus can perform fast, contiguous sets/fills in RAM.
    ///
    /// The interpreter uses this as a hint for REP STOS* fast paths.
    fn supports_bulk_set(&self) -> bool {
        false
    }

    /// Fast fill of `repeat` elements at `dst`, writing `pattern` each time.
    ///
    /// For example, STOSD would call this with `pattern.len() == 4`.
    fn bulk_set(&mut self, _dst: u64, _pattern: &[u8], _repeat: usize) -> bool {
        false
    }
}

/// Simple in-memory RAM bus used by tests.
#[derive(Clone, Debug)]
pub struct RamBus {
    mem: Vec<u8>,
}

impl RamBus {
    pub fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.mem
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.mem
    }

    fn range(&self, addr: u64, len: usize) -> Range<usize> {
        let start: usize = addr
            .try_into()
            .unwrap_or_else(|_| panic!("address out of range: {addr:#x}"));
        let end = start
            .checked_add(len)
            .unwrap_or_else(|| panic!("address overflow: {addr:#x} + {len}"));
        assert!(
            end <= self.mem.len(),
            "RAM access out of bounds: {addr:#x}..{end:#x} (ram_size={:#x})",
            self.mem.len()
        );
        start..end
    }
}

impl Bus for RamBus {
    fn read_u8(&mut self, addr: u64) -> u8 {
        let idx: usize = addr
            .try_into()
            .unwrap_or_else(|_| panic!("address out of range: {addr:#x}"));
        self.mem[idx]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        let idx: usize = addr
            .try_into()
            .unwrap_or_else(|_| panic!("address out of range: {addr:#x}"));
        self.mem[idx] = value;
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> bool {
        if len == 0 {
            return true;
        }
        let src_range = self.range(src, len);
        let dst_range = self.range(dst, len);

        // `copy_within` provides memmove semantics, which is correct for the
        // interpreter fast path (it is only used when overlapping copies are
        // known to be safe given the current DF and addresses).
        if src_range.start == dst_range.start {
            return true;
        }

        // Rust requires an owned range for copy_within, and overlapping ranges
        // are permitted.
        let src_range_owned = src_range.clone();
        self.mem.copy_within(src_range_owned, dst_range.start);
        true
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> bool {
        if repeat == 0 {
            return true;
        }
        assert!(!pattern.is_empty(), "pattern must be non-empty");
        let total = pattern
            .len()
            .checked_mul(repeat)
            .expect("bulk_set length overflow");
        let dst_range = self.range(dst, total);
        let dst_slice = &mut self.mem[dst_range];

        if pattern.len() == 1 {
            dst_slice.fill(pattern[0]);
            return true;
        }

        for chunk in dst_slice.chunks_exact_mut(pattern.len()) {
            chunk.copy_from_slice(pattern);
        }
        true
    }
}
