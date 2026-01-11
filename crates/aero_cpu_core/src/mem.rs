use core::ops::Range;

use crate::exception::Exception;

pub trait CpuBusValue: Copy + PartialEq {
    fn read_from(bus: &mut impl CpuBus, vaddr: u64) -> Result<Self, Exception>;
    fn write_to(bus: &mut impl CpuBus, vaddr: u64, val: Self) -> Result<(), Exception>;
}

impl CpuBusValue for u8 {
    fn read_from(bus: &mut impl CpuBus, vaddr: u64) -> Result<Self, Exception> {
        bus.read_u8(vaddr)
    }

    fn write_to(bus: &mut impl CpuBus, vaddr: u64, val: Self) -> Result<(), Exception> {
        bus.write_u8(vaddr, val)
    }
}

impl CpuBusValue for u16 {
    fn read_from(bus: &mut impl CpuBus, vaddr: u64) -> Result<Self, Exception> {
        bus.read_u16(vaddr)
    }

    fn write_to(bus: &mut impl CpuBus, vaddr: u64, val: Self) -> Result<(), Exception> {
        bus.write_u16(vaddr, val)
    }
}

impl CpuBusValue for u32 {
    fn read_from(bus: &mut impl CpuBus, vaddr: u64) -> Result<Self, Exception> {
        bus.read_u32(vaddr)
    }

    fn write_to(bus: &mut impl CpuBus, vaddr: u64, val: Self) -> Result<(), Exception> {
        bus.write_u32(vaddr, val)
    }
}

impl CpuBusValue for u64 {
    fn read_from(bus: &mut impl CpuBus, vaddr: u64) -> Result<Self, Exception> {
        bus.read_u64(vaddr)
    }

    fn write_to(bus: &mut impl CpuBus, vaddr: u64, val: Self) -> Result<(), Exception> {
        bus.write_u64(vaddr, val)
    }
}

impl CpuBusValue for u128 {
    fn read_from(bus: &mut impl CpuBus, vaddr: u64) -> Result<Self, Exception> {
        bus.read_u128(vaddr)
    }

    fn write_to(bus: &mut impl CpuBus, vaddr: u64, val: Self) -> Result<(), Exception> {
        bus.write_u128(vaddr, val)
    }
}

pub trait CpuBus {
    /// Synchronize any paging/MMU state cached by the bus with the current CPU state.
    ///
    /// The tier-0 interpreter calls this once per instruction boundary, before
    /// instruction fetch, so a paging-aware bus can observe CR0/CR3/CR4/EFER/CPL
    /// updates performed by assist handlers.
    #[inline]
    fn sync(&mut self, _state: &crate::state::CpuState) {}

    /// Invalidate a single linear address translation (INVLPG).
    #[inline]
    fn invlpg(&mut self, _vaddr: u64) {}

    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception>;
    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception>;
    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception>;
    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception>;
    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception>;

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception>;
    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception>;
    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception>;
    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception>;
    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception>;

    /// Perform a read-modify-write cycle as a single operation when possible.
    ///
    /// This is used by the Tier-0 interpreter for `LOCK`ed instructions and other
    /// atomic RMW operations (e.g. `XCHG` with a memory operand).
    ///
    /// Bus implementations may override this to provide true atomicity against
    /// concurrent devices/threads. The default implementation falls back to a
    /// plain read + conditional write using the scalar `read_u*`/`write_u*`
    /// operations.
    fn atomic_rmw<T, R>(&mut self, vaddr: u64, f: impl FnOnce(T) -> (T, R)) -> Result<R, Exception>
    where
        T: CpuBusValue,
        Self: Sized,
    {
        let old = T::read_from(self, vaddr)?;
        let (new, ret) = f(old);
        if new != old {
            T::write_to(self, vaddr, new)?;
        }
        Ok(ret)
    }

    /// Read a contiguous byte slice from memory.
    ///
    /// This is primarily a convenience helper for instructions that naturally
    /// operate on byte arrays (FXSAVE/FXRSTOR, REP string ops). Implementations
    /// may override this for more efficient access, but the default
    /// implementation safely falls back to scalar `read_u8` accesses.
    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        for (i, slot) in dst.iter_mut().enumerate() {
            let addr = vaddr
                .checked_add(i as u64)
                .ok_or(Exception::MemoryFault)?;
            *slot = self.read_u8(addr)?;
        }
        Ok(())
    }

    /// Write a contiguous byte slice into memory.
    ///
    /// This is a hint/fast-path only; the default implementation safely falls
    /// back to scalar `write_u8` accesses.
    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        for (i, byte) in src.iter().copied().enumerate() {
            let addr = vaddr
                .checked_add(i as u64)
                .ok_or(Exception::MemoryFault)?;
            self.write_u8(addr, byte)?;
        }
        Ok(())
    }

    /// Whether this bus can perform fast contiguous copies between RAM regions.
    ///
    /// This is a hint only: callers may still invoke [`CpuBus::bulk_copy`] when
    /// this returns `false` and will get correct results (via the default scalar
    /// fallback implementation), but Tier-0 fast paths will typically avoid
    /// attempting the bulk call in that case.
    fn supports_bulk_copy(&self) -> bool {
        false
    }

    /// Copy `len` bytes from `src` to `dst` with memmove semantics.
    ///
    /// Returns `true` when the copy was performed.
    ///
    /// The default implementation performs a byte-at-a-time copy and is correct
    /// but potentially slow.
    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }

        let len_u64 = len as u64;
        let src_end = src.checked_add(len_u64).ok_or(Exception::MemoryFault)?;
        let dst_end = dst.checked_add(len_u64).ok_or(Exception::MemoryFault)?;

        let overlap = src < dst_end && dst < src_end;
        let copy_backward = overlap && dst > src;

        if copy_backward {
            for i in (0..len).rev() {
                let b = self.read_u8(src + i as u64)?;
                self.write_u8(dst + i as u64, b)?;
            }
        } else {
            for i in 0..len {
                let b = self.read_u8(src + i as u64)?;
                self.write_u8(dst + i as u64, b)?;
            }
        }

        Ok(true)
    }

    /// Whether this bus can perform fast contiguous repeated sets/fills in RAM.
    fn supports_bulk_set(&self) -> bool {
        false
    }

    /// Fill `repeat` elements at `dst`, writing `pattern` each time.
    ///
    /// Returns `true` when the fill was performed.
    ///
    /// The default implementation performs a byte-at-a-time fill and is correct
    /// but potentially slow.
    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 || pattern.is_empty() {
            return Ok(true);
        }

        let total = pattern
            .len()
            .checked_mul(repeat)
            .ok_or(Exception::MemoryFault)?;

        // Bounds-check destination range without panicking on overflow.
        let total_u64 = total as u64;
        dst.checked_add(total_u64).ok_or(Exception::MemoryFault)?;

        for i in 0..total {
            let b = pattern[i % pattern.len()];
            self.write_u8(dst + i as u64, b)?;
        }

        Ok(true)
    }

    /// Fetch up to 15 bytes from instruction memory. Implementations should
    /// allow reads that cross page boundaries (the caller handles page faults
    /// separately), but for tests we just bounds-check.
    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception>;

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception>;
    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception>;
}

/// Identity-mapped memory bus used by unit tests.
#[derive(Debug, Clone)]
pub struct FlatTestBus {
    mem: Vec<u8>,
}

impl FlatTestBus {
    pub fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn range(&self, addr: u64, len: usize) -> Result<Range<usize>, Exception> {
        let start = usize::try_from(addr).map_err(|_| Exception::MemoryFault)?;
        let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
        if end > self.mem.len() {
            return Err(Exception::MemoryFault);
        }
        Ok(start..end)
    }

    pub fn load(&mut self, addr: u64, data: &[u8]) {
        let range = self
            .range(addr, data.len())
            .unwrap_or_else(|_| panic!("FlatTestBus load out of bounds: {addr:#x}+{}", data.len()));
        self.mem[range].copy_from_slice(data);
    }

    pub fn slice(&self, addr: u64, len: usize) -> &[u8] {
        let range = self
            .range(addr, len)
            .unwrap_or_else(|_| panic!("FlatTestBus slice out of bounds: {addr:#x}+{len}"));
        &self.mem[range]
    }
}

impl CpuBus for FlatTestBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        let idx = usize::try_from(vaddr).map_err(|_| Exception::MemoryFault)?;
        self.mem.get(idx).copied().ok_or(Exception::MemoryFault)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let mut buf = [0u8; 2];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut buf = [0u8; 4];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut buf = [0u8; 8];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut buf = [0u8; 16];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        let idx = usize::try_from(vaddr).map_err(|_| Exception::MemoryFault)?;
        let slot = self.mem.get_mut(idx).ok_or(Exception::MemoryFault)?;
        *slot = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        let range = self.range(vaddr, dst.len())?;
        dst.copy_from_slice(&self.mem[range]);
        Ok(())
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        let range = self.range(vaddr, src.len())?;
        self.mem[range].copy_from_slice(src);
        Ok(())
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }
        let src_range = self.range(src, len)?;
        let dst_range = self.range(dst, len)?;
        if src_range.start == dst_range.start {
            return Ok(true);
        }
        self.mem.copy_within(src_range.clone(), dst_range.start);
        Ok(true)
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 || pattern.is_empty() {
            return Ok(true);
        }

        let total = pattern
            .len()
            .checked_mul(repeat)
            .ok_or(Exception::MemoryFault)?;
        let range = self.range(dst, total)?;
        let dst_slice = &mut self.mem[range];

        if pattern.len() == 1 {
            dst_slice.fill(pattern[0]);
            return Ok(true);
        }

        for chunk in dst_slice.chunks_exact_mut(pattern.len()) {
            chunk.copy_from_slice(pattern);
        }

        Ok(true)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        self.read_bytes(vaddr, &mut buf[..len])?;
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}
