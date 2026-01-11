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

    /// Fetch up to 15 bytes from instruction memory. Implementations should
    /// allow reads that cross page boundaries (the caller handles page faults
    /// separately), but for tests we just bounds-check.
    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception>;

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception>;
    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception>;

    /// Perform a read-modify-write cycle as a single operation when possible.
    ///
    /// This is used by Tier-0 for `LOCK`ed instructions and other atomic RMW
    /// instructions (for example, `CMPXCHG`, `XADD`, and `XCHG` with a memory
    /// operand).
    ///
    /// Implementations may override this to provide true atomicity against
    /// concurrent devices/threads. The default implementation falls back to a
    /// plain read + conditional write.
    fn atomic_rmw<T, R>(
        &mut self,
        addr: u64,
        f: impl FnOnce(T) -> (T, R),
    ) -> Result<R, Exception>
    where
        T: CpuBusValue,
        Self: Sized,
    {
        let old = T::read_from(self, addr)?;
        let (new, ret) = f(old);
        if new != old {
            T::write_to(self, addr, new)?;
        }
        Ok(ret)
    }

    /// Whether this bus can perform fast contiguous copies between RAM regions.
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
    /// The default implementation performs a byte-at-a-time copy and is correct
    /// but potentially slow.
    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 {
            return Ok(true);
        }
        if pattern.is_empty() {
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

    pub fn load(&mut self, addr: u64, data: &[u8]) {
        let start = addr as usize;
        let end = start + data.len();
        self.mem[start..end].copy_from_slice(data);
    }

    pub fn slice(&self, addr: u64, len: usize) -> &[u8] {
        let start = addr as usize;
        let end = start + len;
        &self.mem[start..end]
    }

    fn range(&self, addr: u64, len: usize) -> Result<Range<usize>, Exception> {
        let start = usize::try_from(addr).map_err(|_| Exception::MemoryFault)?;
        let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
        if end > self.mem.len() {
            return Err(Exception::MemoryFault);
        }
        Ok(start..end)
    }
}

impl CpuBus for FlatTestBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.mem
            .get(vaddr as usize)
            .copied()
            .ok_or(Exception::MemoryFault)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let lo = self.read_u8(vaddr)? as u16;
        let hi = self.read_u8(vaddr + 1)? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut v = 0u32;
        for i in 0..4 {
            v |= (self.read_u8(vaddr + i)? as u32) << (i * 8);
        }
        Ok(v)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut v = 0u64;
        for i in 0..8 {
            v |= (self.read_u8(vaddr + i)? as u64) << (i * 8);
        }
        Ok(v)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut v = 0u128;
        for i in 0..16 {
            v |= (self.read_u8(vaddr + i)? as u128) << (i * 8);
        }
        Ok(v)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        let slot = self
            .mem
            .get_mut(vaddr as usize)
            .ok_or(Exception::MemoryFault)?;
        *slot = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_u8(vaddr, (val & 0xFF) as u8)?;
        self.write_u8(vaddr + 1, (val >> 8) as u8)?;
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        for i in 0..4 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        for i in 0..8 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        for i in 0..16 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = self.read_u8(vaddr + i as u64)?;
        }
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
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

        let src_range_owned = src_range.clone();
        self.mem.copy_within(src_range_owned, dst_range.start);
        Ok(true)
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 {
            return Ok(true);
        }
        if pattern.is_empty() {
            return Ok(true);
        }

        let total = pattern
            .len()
            .checked_mul(repeat)
            .ok_or(Exception::MemoryFault)?;
        let dst_range = self.range(dst, total)?;
        let dst_slice = &mut self.mem[dst_range];

        if pattern.len() == 1 {
            dst_slice.fill(pattern[0]);
            return Ok(true);
        }

        for chunk in dst_slice.chunks_exact_mut(pattern.len()) {
            chunk.copy_from_slice(pattern);
        }

        Ok(true)
    }
}
