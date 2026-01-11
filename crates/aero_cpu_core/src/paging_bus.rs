use crate::exception::Exception;
use crate::mem::CpuBus;
use aero_mmu::{AccessType, MemoryBus, Mmu, TranslateFault};

/// A paging-aware [`CpuBus`] implementation backed by [`aero_mmu::Mmu`].
///
/// The tier-0 interpreter passes *linear* addresses to [`CpuBus`] methods. This
/// adapter translates them to physical addresses via `aero-mmu` before accessing
/// the underlying physical bus `B`.
#[derive(Debug)]
pub struct PagingBus<B> {
    mmu: Mmu,
    phys: B,
    cpl: u8,
    write_chunks: Vec<(u64, usize, usize)>,
}

const PAGE_SIZE: u64 = 4096;

impl<B> PagingBus<B> {
    pub fn new(phys: B) -> Self {
        Self {
            mmu: Mmu::new(),
            phys,
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
struct WriteIntent<'a, B> {
    bus: &'a mut PagingBus<B>,
}

impl<B: MemoryBus> CpuBus for WriteIntent<'_, B> {
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
        let mut buf = [0u8; 2];
        self.bus.read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut buf = [0u8; 4];
        self.bus.read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut buf = [0u8; 8];
        self.bus.read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut buf = [0u8; 16];
        self.bus.read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.bus.write_u8_access(vaddr, AccessType::Write, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.bus
            .write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.bus
            .write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.bus
            .write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
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
}

impl<B> CpuBus for PagingBus<B>
where
    B: MemoryBus,
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
        let mut buf = [0u8; 2];
        self.read_bytes_access(vaddr, &mut buf, AccessType::Read)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut buf = [0u8; 4];
        self.read_bytes_access(vaddr, &mut buf, AccessType::Read)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut buf = [0u8; 8];
        self.read_bytes_access(vaddr, &mut buf, AccessType::Read)?;
        Ok(u64::from_le_bytes(buf))
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
        self.write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.write_bytes_access(vaddr, &val.to_le_bytes(), AccessType::Write)
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

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}
