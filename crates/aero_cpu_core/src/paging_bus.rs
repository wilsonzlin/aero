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
}

impl<B> PagingBus<B> {
    pub fn new(phys: B) -> Self {
        Self {
            mmu: Mmu::new(),
            phys,
            cpl: 0,
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
    fn write_u8_access(&mut self, vaddr: u64, access: AccessType, value: u8) -> Result<(), Exception>
    where
        B: MemoryBus,
    {
        let paddr = self.translate(vaddr, access)?;
        self.phys.write_u8(paddr, value);
        Ok(())
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
        let b0 = self.read_u8(vaddr)? as u16;
        let b1 = self.read_u8(vaddr + 1)? as u16;
        Ok(b0 | (b1 << 8))
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
        self.write_u8_access(vaddr, AccessType::Write, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_u8(vaddr, (val & 0xff) as u8)?;
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
            buf[i] = self.read_u8_access(vaddr + i as u64, AccessType::Execute)?;
        }
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Ok(())
    }
}
