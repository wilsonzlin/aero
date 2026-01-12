use crate::exception::Exception;
use crate::mem::CpuBus;
use aero_mmu::{AccessType, MemoryBus, Mmu, TranslateFault};
use core::fmt;

const SCRATCH_SIZE: usize = 4096;

/// A paging-aware [`CpuBus`] implementation backed by [`aero_mmu::Mmu`].
///
/// The tier-0 interpreter passes *linear* addresses to [`CpuBus`] methods. This
/// adapter translates them to physical addresses via `aero-mmu` before accessing
/// the underlying physical bus `B`.
pub trait IoBus {
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception>;
    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception>;
}

/// Default port-I/O backend that behaves like the old `PagingBus` stub I/O: all
/// reads return `0` and all writes are ignored.
#[derive(Clone, Copy, Default)]
pub struct NoIo;

impl IoBus for NoIo {
    #[inline]
    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    #[inline]
    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}

pub struct PagingBus<B, IO = NoIo> {
    mmu: Mmu,
    phys: B,
    io: IO,
    cpl: u8,
    write_chunks: Vec<(u64, usize, usize)>,
}

const PAGE_SIZE: u64 = 4096;

/// Wrapper that forwards MMU page-walk reads but discards writes.
///
/// This is used by `PagingBus` bulk-ops preflight to ensure that declining a bulk
/// optimization (`Ok(false)`) does not mutate guest-visible state (e.g. paging
/// accessed/dirty bits) or internal MMU state (we run translation against a
/// cloned MMU).
struct NoWriteBus<'a, B> {
    inner: &'a mut B,
}

impl<B: MemoryBus> MemoryBus for NoWriteBus<'_, B> {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.inner.read_u8(paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        self.inner.read_u16(paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        self.inner.read_u32(paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        self.inner.read_u64(paddr)
    }

    #[inline]
    fn write_u8(&mut self, _paddr: u64, _value: u8) {}

    #[inline]
    fn write_u16(&mut self, _paddr: u64, _value: u16) {}

    #[inline]
    fn write_u32(&mut self, _paddr: u64, _value: u32) {}

    #[inline]
    fn write_u64(&mut self, _paddr: u64, _value: u64) {}
}

impl<B> PagingBus<B, NoIo> {
    pub fn new(phys: B) -> PagingBus<B, NoIo> {
        PagingBus::new_with_io(phys, NoIo)
    }
}

impl<B, IO> PagingBus<B, IO> {
    pub fn new_with_io(phys: B, io: IO) -> PagingBus<B, IO> {
        Self {
            mmu: Mmu::new(),
            phys,
            io,
            cpl: 0,
            write_chunks: Vec::new(),
        }
    }

    #[inline]
    pub fn mmu(&self) -> &Mmu {
        &self.mmu
    }

    #[inline]
    pub fn mmu_mut(&mut self) -> &mut Mmu {
        &mut self.mmu
    }

    #[inline]
    pub fn into_inner(self) -> B {
        self.phys
    }

    #[inline]
    pub fn inner(&self) -> &B {
        &self.phys
    }

    #[inline]
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.phys
    }

    fn preflight_range_with_mmu(
        &mut self,
        mmu: &mut Mmu,
        vaddr: u64,
        len: usize,
        access: AccessType,
    ) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        if len == 0 {
            return Ok(());
        }

        let mut phys = NoWriteBus {
            inner: &mut self.phys,
        };

        let mut offset = 0usize;
        while offset < len {
            let addr = vaddr.wrapping_add(offset as u64);
            match mmu.translate(&mut phys, addr, access, self.cpl) {
                Ok(_paddr) => {}
                Err(TranslateFault::PageFault(pf)) => {
                    return Err(Exception::PageFault {
                        addr: pf.addr,
                        error_code: pf.error_code,
                    });
                }
                Err(TranslateFault::NonCanonical(_addr)) => return Err(Exception::gp0()),
            }

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(len - offset);
            offset += chunk_len;
        }

        Ok(())
    }

    #[inline]
    pub fn io(&self) -> &IO {
        &self.io
    }

    #[inline]
    pub fn io_mut(&mut self) -> &mut IO {
        &mut self.io
    }

    #[inline]
    fn translate(&mut self, vaddr: u64, access: AccessType) -> Result<u64, Exception>
    where
        B: MemoryBus,
    {
        match self.mmu.translate(&mut self.phys, vaddr, access, self.cpl) {
            Ok(paddr) => Ok(paddr),
            Err(TranslateFault::PageFault(pf)) => Err(Exception::PageFault {
                addr: pf.addr,
                error_code: pf.error_code,
            }),
            Err(TranslateFault::NonCanonical(_addr)) => Err(Exception::gp0()),
        }
    }

    #[inline]
    fn read_u8_access(&mut self, vaddr: u64, access: AccessType) -> Result<u8, Exception>
    where
        B: MemoryBus,
    {
        let paddr = self.translate(vaddr, access)?;
        Ok(self.phys.read_u8(paddr))
    }

    #[inline]
    fn read_u16_access(&mut self, vaddr: u64, access: AccessType) -> Result<u16, Exception>
    where
        B: MemoryBus,
    {
        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off <= PAGE_SIZE - 2 {
            let paddr = self.translate(vaddr, access)?;
            return Ok(self.phys.read_u16(paddr));
        }

        let mut buf = [0u8; 2];
        self.read_bytes_access(vaddr, &mut buf, access)?;
        Ok(u16::from_le_bytes(buf))
    }

    #[inline]
    fn read_u32_access(&mut self, vaddr: u64, access: AccessType) -> Result<u32, Exception>
    where
        B: MemoryBus,
    {
        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off <= PAGE_SIZE - 4 {
            let paddr = self.translate(vaddr, access)?;
            return Ok(self.phys.read_u32(paddr));
        }

        let mut buf = [0u8; 4];
        self.read_bytes_access(vaddr, &mut buf, access)?;
        Ok(u32::from_le_bytes(buf))
    }

    #[inline]
    fn read_u64_access(&mut self, vaddr: u64, access: AccessType) -> Result<u64, Exception>
    where
        B: MemoryBus,
    {
        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off <= PAGE_SIZE - 8 {
            let paddr = self.translate(vaddr, access)?;
            return Ok(self.phys.read_u64(paddr));
        }

        let mut buf = [0u8; 8];
        self.read_bytes_access(vaddr, &mut buf, access)?;
        Ok(u64::from_le_bytes(buf))
    }

    #[inline]
    fn write_u8_access(
        &mut self,
        vaddr: u64,
        access: AccessType,
        value: u8,
    ) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        let paddr = self.translate(vaddr, access)?;
        self.phys.write_u8(paddr, value);
        Ok(())
    }

    #[inline]
    fn write_u16_access(
        &mut self,
        vaddr: u64,
        access: AccessType,
        value: u16,
    ) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off <= PAGE_SIZE - 2 {
            let paddr = self.translate(vaddr, access)?;
            self.phys.write_u16(paddr, value);
            return Ok(());
        }

        self.write_bytes_access(vaddr, &value.to_le_bytes(), access)
    }

    #[inline]
    fn write_u32_access(
        &mut self,
        vaddr: u64,
        access: AccessType,
        value: u32,
    ) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off <= PAGE_SIZE - 4 {
            let paddr = self.translate(vaddr, access)?;
            self.phys.write_u32(paddr, value);
            return Ok(());
        }

        self.write_bytes_access(vaddr, &value.to_le_bytes(), access)
    }

    #[inline]
    fn write_u64_access(
        &mut self,
        vaddr: u64,
        access: AccessType,
        value: u64,
    ) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off <= PAGE_SIZE - 8 {
            let paddr = self.translate(vaddr, access)?;
            self.phys.write_u64(paddr, value);
            return Ok(());
        }

        self.write_bytes_access(vaddr, &value.to_le_bytes(), access)
    }

    fn read_bytes_access(
        &mut self,
        vaddr: u64,
        dst: &mut [u8],
        access: AccessType,
    ) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        if dst.is_empty() {
            return Ok(());
        }

        let mut offset = 0usize;
        while offset < dst.len() {
            let addr = vaddr.wrapping_add(offset as u64);
            let paddr = self.translate(addr, access)?;

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(dst.len() - offset);

            for i in 0..chunk_len {
                dst[offset + i] = self.phys.read_u8(paddr.wrapping_add(i as u64));
            }

            offset += chunk_len;
        }

        Ok(())
    }

    fn write_bytes_access(
        &mut self,
        vaddr: u64,
        src: &[u8],
        access: AccessType,
    ) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        if src.is_empty() {
            return Ok(());
        }

        self.write_chunks.clear();
        let mut offset = 0usize;
        while offset < src.len() {
            let addr = vaddr.wrapping_add(offset as u64);
            let paddr = self.translate(addr, access)?;

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(src.len() - offset);

            self.write_chunks.push((paddr, chunk_len, offset));
            offset += chunk_len;
        }

        let phys = &mut self.phys;
        for (paddr, len, src_off) in self.write_chunks.iter().copied() {
            for i in 0..len {
                phys.write_u8(paddr.wrapping_add(i as u64), src[src_off + i]);
            }
        }

        Ok(())
    }
}

/// Adapter that performs reads with write-intent access checks.
///
/// Tier-0 uses [`CpuBus::atomic_rmw`] to model `LOCK`ed RMW instructions. Even if
/// the computed new value equals the old one, real hardware still performs a
/// write-intent access (and thus may set accessed/dirty bits and fault on
/// read-only pages). By performing reads with [`AccessType::Write`], we ensure
/// those permission checks happen.
struct WriteIntent<'a, B, IO> {
    bus: &'a mut PagingBus<B, IO>,
}

impl<B: MemoryBus, IO: IoBus> CpuBus for WriteIntent<'_, B, IO> {
    fn sync(&mut self, state: &crate::state::CpuState) {
        self.bus.sync(state);
    }

    fn invlpg(&mut self, vaddr: u64) {
        self.bus.invlpg(vaddr);
    }

    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.bus.read_u8_access(vaddr, AccessType::Write)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.bus.read_u16_access(vaddr, AccessType::Write)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.bus.read_u32_access(vaddr, AccessType::Write)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.bus.read_u64_access(vaddr, AccessType::Write)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut buf = [0u8; 16];
        self.bus
            .read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.bus.write_u8_access(vaddr, AccessType::Write, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.bus.write_u16_access(vaddr, AccessType::Write, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.bus.write_u32_access(vaddr, AccessType::Write, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.bus.write_u64_access(vaddr, AccessType::Write, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.bus
            .write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.bus.read_bytes_access(vaddr, dst, AccessType::Write)
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.bus.write_bytes_access(vaddr, src, AccessType::Write)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.bus.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.bus.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.bus.io_write(port, size, val)
    }

    fn supports_bulk_copy(&self) -> bool {
        self.bus.supports_bulk_copy()
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        self.bus.bulk_copy(dst, src, len)
    }

    fn supports_bulk_set(&self) -> bool {
        self.bus.supports_bulk_set()
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        self.bus.bulk_set(dst, pattern, repeat)
    }
}

impl<B, IO> CpuBus for PagingBus<B, IO>
where
    B: MemoryBus,
    IO: IoBus,
{
    fn sync(&mut self, state: &crate::state::CpuState) {
        state.sync_mmu(&mut self.mmu);
        self.cpl = state.cpl();
    }

    fn invlpg(&mut self, vaddr: u64) {
        self.mmu.invlpg(vaddr);
    }

    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.read_u8_access(vaddr, AccessType::Read)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.read_u16_access(vaddr, AccessType::Read)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.read_u32_access(vaddr, AccessType::Read)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.read_u64_access(vaddr, AccessType::Read)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut buf = [0u8; 16];
        self.read_bytes_access(vaddr, &mut buf, AccessType::Read)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.write_u8_access(vaddr, AccessType::Write, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_u16_access(vaddr, AccessType::Write, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.write_u32_access(vaddr, AccessType::Write, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.write_u64_access(vaddr, AccessType::Write, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.read_bytes_access(vaddr, dst, AccessType::Read)
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.write_bytes_access(vaddr, src, AccessType::Write)
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        // Translate the full range with write intent, but do not touch the target bytes.
        // This allows higher-level helpers (e.g. wrapped multi-byte writes) to remain
        // atomic w.r.t #PF even when the access must be split into multiple segments.
        if len == 0 {
            return Ok(());
        }

        let mut offset = 0usize;
        while offset < len {
            let addr = vaddr.wrapping_add(offset as u64);
            let _paddr = self.translate(addr, AccessType::Write)?;

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(len - offset);

            offset += chunk_len;
        }

        Ok(())
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }

        // Bounds-check ranges without panicking on overflow.
        let len_u64 = u64::try_from(len).map_err(|_| Exception::MemoryFault)?;
        src.checked_add(len_u64).ok_or(Exception::MemoryFault)?;
        dst.checked_add(len_u64).ok_or(Exception::MemoryFault)?;

        // Preflight translations with a side-effect-free probe. If any page in
        // the range is unmapped or fails permission checks, decline the bulk
        // operation so Tier-0 can fall back to scalar accesses (which will
        // perform architecturally correct A/D bit updates and fault delivery).
        let mut offset = 0usize;
        while offset < len {
            let addr_off = offset as u64;
            let src_addr = src.wrapping_add(addr_off);
            let dst_addr = dst.wrapping_add(addr_off);

            if self
                .mmu
                .translate_probe(&mut self.phys, src_addr, AccessType::Read, self.cpl)
                .is_err()
            {
                return Ok(false);
            }
            if self
                .mmu
                .translate_probe(&mut self.phys, dst_addr, AccessType::Write, self.cpl)
                .is_err()
            {
                return Ok(false);
            }

            let src_page_off = (src_addr & (PAGE_SIZE - 1)) as usize;
            let dst_page_off = (dst_addr & (PAGE_SIZE - 1)) as usize;
            let src_page_rem = (PAGE_SIZE as usize) - src_page_off;
            let dst_page_rem = (PAGE_SIZE as usize) - dst_page_off;
            let chunk_len = src_page_rem.min(dst_page_rem).min(len - offset);
            offset += chunk_len;
        }

        // Perform the copy with memmove semantics using a bounded scratch buffer.
        let len_u64 = len_u64; // keep for overlap calculation
        let src_end = src.wrapping_add(len_u64);
        let dst_end = dst.wrapping_add(len_u64);
        let overlap = src < dst_end && dst < src_end;
        let copy_backward = overlap && dst > src;

        // 4KiB buffer keeps allocations bounded even for large REP MOVS*.
        let mut buf = vec![0u8; PAGE_SIZE as usize];

        if copy_backward {
            let mut remaining = len;
            while remaining != 0 {
                let chunk_len = remaining.min(buf.len());
                let chunk_start = remaining - chunk_len;
                self.read_bytes_access(
                    src.wrapping_add(chunk_start as u64),
                    &mut buf[..chunk_len],
                    AccessType::Read,
                )?;
                self.write_bytes_access(
                    dst.wrapping_add(chunk_start as u64),
                    &buf[..chunk_len],
                    AccessType::Write,
                )?;
                remaining = chunk_start;
            }
        } else {
            let mut done = 0usize;
            while done < len {
                let chunk_len = (len - done).min(buf.len());
                self.read_bytes_access(
                    src.wrapping_add(done as u64),
                    &mut buf[..chunk_len],
                    AccessType::Read,
                )?;
                self.write_bytes_access(
                    dst.wrapping_add(done as u64),
                    &buf[..chunk_len],
                    AccessType::Write,
                )?;
                done += chunk_len;
            }
        }

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
        let total_u64 = u64::try_from(total).map_err(|_| Exception::MemoryFault)?;
        dst.checked_add(total_u64).ok_or(Exception::MemoryFault)?;

        // Preflight translations with write intent using a side-effect-free probe.
        let mut offset = 0usize;
        while offset < total {
            let addr = dst.wrapping_add(offset as u64);
            if self
                .mmu
                .translate_probe(&mut self.phys, addr, AccessType::Write, self.cpl)
                .is_err()
            {
                return Ok(false);
            }

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(total - offset);
            offset += chunk_len;
        }

        // Commit writes in bounded chunks.
        let mut buf = vec![0u8; PAGE_SIZE as usize];
        let mut done = 0usize;
        while done < total {
            let chunk_len = (total - done).min(buf.len());
            for i in 0..chunk_len {
                buf[i] = pattern[(done + i) % pattern.len()];
            }
            self.write_bytes_access(
                dst.wrapping_add(done as u64),
                &buf[..chunk_len],
                AccessType::Write,
            )?;
            done += chunk_len;
        }

        Ok(true)
    }

    fn atomic_rmw<T, R>(&mut self, vaddr: u64, f: impl FnOnce(T) -> (T, R)) -> Result<R, Exception>
    where
        T: crate::mem::CpuBusValue,
        Self: Sized,
    {
        // Perform the read with write-intent translation so that permission
        // checks and accessed/dirty bit updates match real RMW semantics.
        let old = {
            let mut intent = WriteIntent { bus: self };
            T::read_from(&mut intent, vaddr)?
        };
        let (new, ret) = f(old);
        if new != old {
            let mut intent = WriteIntent { bus: self };
            T::write_to(&mut intent, vaddr, new)?;
        }
        Ok(ret)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        self.read_bytes_access(vaddr, &mut buf[..len], AccessType::Execute)?;
        Ok(buf)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.io.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.io.io_write(port, size, val)
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }

        // Bounds-check ranges up-front so overlap detection doesn't panic/overflow.
        let len_u64 = u64::try_from(len).map_err(|_| Exception::MemoryFault)?;
        let src_end = src.checked_add(len_u64).ok_or(Exception::MemoryFault)?;
        let dst_end = dst.checked_add(len_u64).ok_or(Exception::MemoryFault)?;

        // Translation preflight against a cloned MMU and a write-suppressing bus so
        // that returning `Ok(false)` never mutates guest-visible state (paging A/D
        // bits, CR2, etc).
        let mut mmu = self.mmu.clone();
        if let Err(e) = self.preflight_range_with_mmu(&mut mmu, src, len, AccessType::Read) {
            if matches!(e, Exception::PageFault { .. } | Exception::GeneralProtection(_)) {
                return Ok(false);
            }
            return Err(e);
        }
        if let Err(e) = self.preflight_range_with_mmu(&mut mmu, dst, len, AccessType::Write) {
            if matches!(e, Exception::PageFault { .. } | Exception::GeneralProtection(_)) {
                return Ok(false);
            }
            return Err(e);
        }

        let overlap = src < dst_end && dst < src_end;
        let copy_backward = overlap && dst > src;

        let mut scratch = [0u8; SCRATCH_SIZE];

        if copy_backward {
            let mut remaining = len;
            while remaining != 0 {
                let chunk_len = SCRATCH_SIZE.min(remaining);
                let offset = remaining - chunk_len;

                let src_addr = src.wrapping_add(offset as u64);
                let dst_addr = dst.wrapping_add(offset as u64);

                self.read_bytes_access(src_addr, &mut scratch[..chunk_len], AccessType::Read)?;
                self.write_bytes_access(dst_addr, &scratch[..chunk_len], AccessType::Write)?;

                remaining = offset;
            }
        } else {
            let mut offset = 0usize;
            while offset < len {
                let chunk_len = SCRATCH_SIZE.min(len - offset);

                let src_addr = src.wrapping_add(offset as u64);
                let dst_addr = dst.wrapping_add(offset as u64);

                self.read_bytes_access(src_addr, &mut scratch[..chunk_len], AccessType::Read)?;
                self.write_bytes_access(dst_addr, &scratch[..chunk_len], AccessType::Write)?;

                offset += chunk_len;
            }
        }

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

        // Bounds-check destination range without panicking on overflow.
        let total_u64 = u64::try_from(total).map_err(|_| Exception::MemoryFault)?;
        dst.checked_add(total_u64).ok_or(Exception::MemoryFault)?;

        let mut mmu = self.mmu.clone();
        if let Err(e) = self.preflight_range_with_mmu(&mut mmu, dst, total, AccessType::Write) {
            if matches!(e, Exception::PageFault { .. } | Exception::GeneralProtection(_)) {
                return Ok(false);
            }
            return Err(e);
        }

        let mut scratch = [0u8; SCRATCH_SIZE];
        let pat_len = pattern.len();

        let mut written = 0usize;
        while written < total {
            let chunk_len = SCRATCH_SIZE.min(total - written);
            let pat_off = written % pat_len;

            if pat_len == 1 {
                scratch[..chunk_len].fill(pattern[0]);
            } else {
                for i in 0..chunk_len {
                    scratch[i] = pattern[(pat_off + i) % pat_len];
                }
            }

            let addr = dst.wrapping_add(written as u64);
            self.write_bytes_access(addr, &scratch[..chunk_len], AccessType::Write)?;
            written += chunk_len;
        }

        Ok(true)
    }
}

impl<B: fmt::Debug, IO> fmt::Debug for PagingBus<B, IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PagingBus")
            .field("mmu", &self.mmu)
            .field("cpl", &self.cpl)
            .field("phys", &self.phys)
            // Avoid requiring `IO: Debug` (real backends often aren't).
            .field("io", &core::any::type_name::<IO>())
            .finish()
    }
}
