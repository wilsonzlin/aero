use crate::PcPlatform;
use aero_cpu_core::interrupts::InterruptController as CpuInterruptController;
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::Exception;
use aero_mmu::{AccessType, MemoryBus as MmuBus, Mmu, TranslateFault};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, SharedPlatformInterrupts,
};
use memory::MemoryBus as PhysicalMemoryBus;

const PAGE_SIZE: u64 = 4096;

struct PhysBus<'a>(&'a mut aero_platform::memory::MemoryBus);

impl MmuBus for PhysBus<'_> {
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.0.read_u8(paddr)
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        PhysicalMemoryBus::read_u16(self.0, paddr)
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        PhysicalMemoryBus::read_u32(self.0, paddr)
    }

    fn read_u64(&mut self, paddr: u64) -> u64 {
        PhysicalMemoryBus::read_u64(self.0, paddr)
    }

    fn write_u8(&mut self, paddr: u64, value: u8) {
        self.0.write_u8(paddr, value);
    }

    fn write_u16(&mut self, paddr: u64, value: u16) {
        PhysicalMemoryBus::write_u16(self.0, paddr, value);
    }

    fn write_u32(&mut self, paddr: u64, value: u32) {
        PhysicalMemoryBus::write_u32(self.0, paddr, value);
    }

    fn write_u64(&mut self, paddr: u64, value: u64) {
        PhysicalMemoryBus::write_u64(self.0, paddr, value);
    }
}

pub struct PcInterruptController {
    interrupts: SharedPlatformInterrupts,
}

impl PcInterruptController {
    pub fn new(interrupts: SharedPlatformInterrupts) -> Self {
        Self { interrupts }
    }
}

impl CpuInterruptController for PcInterruptController {
    fn poll_interrupt(&mut self) -> Option<u8> {
        let mut interrupts = self.interrupts.borrow_mut();
        let vector = PlatformInterruptController::get_pending(&*interrupts)?;
        PlatformInterruptController::acknowledge(&mut *interrupts, vector);
        Some(vector)
    }
}

pub struct PcCpuBus {
    pub platform: PcPlatform,
    mmu: Mmu,
    cpl: u8,
    write_chunks: Vec<(u64, usize, usize)>,
}

impl PcCpuBus {
    pub fn new(platform: PcPlatform) -> Self {
        Self {
            platform,
            mmu: Mmu::new(),
            cpl: 0,
            write_chunks: Vec::new(),
        }
    }

    pub fn interrupt_controller(&self) -> PcInterruptController {
        PcInterruptController::new(self.platform.interrupts.clone())
    }

    pub fn mmu(&self) -> &Mmu {
        &self.mmu
    }

    pub fn mmu_mut(&mut self) -> &mut Mmu {
        &mut self.mmu
    }

    pub fn into_platform(self) -> PcPlatform {
        self.platform
    }

    fn translate(&mut self, vaddr: u64, access: AccessType) -> Result<u64, Exception> {
        let mut phys = PhysBus(&mut self.platform.memory);
        match self.mmu.translate(&mut phys, vaddr, access, self.cpl) {
            Ok(paddr) => Ok(paddr),
            Err(TranslateFault::PageFault(pf)) => Err(Exception::PageFault {
                addr: pf.addr,
                error_code: pf.error_code,
            }),
            Err(TranslateFault::NonCanonical(_)) => Err(Exception::gp0()),
        }
    }

    fn read_u8_access(&mut self, vaddr: u64, access: AccessType) -> Result<u8, Exception> {
        let paddr = self.translate(vaddr, access)?;
        Ok(self.platform.memory.read_u8(paddr))
    }

    fn write_u8_access(
        &mut self,
        vaddr: u64,
        access: AccessType,
        value: u8,
    ) -> Result<(), Exception> {
        let paddr = self.translate(vaddr, access)?;
        self.platform.memory.write_u8(paddr, value);
        Ok(())
    }

    fn read_bytes_access(
        &mut self,
        vaddr: u64,
        dst: &mut [u8],
        access: AccessType,
    ) -> Result<(), Exception> {
        if dst.is_empty() {
            return Ok(());
        }

        let mut offset = 0usize;
        while offset < dst.len() {
            let addr = vaddr
                .checked_add(offset as u64)
                .ok_or(Exception::MemoryFault)?;
            let paddr = self.translate(addr, access)?;

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(dst.len() - offset);

            self.platform
                .memory
                .read_physical(paddr, &mut dst[offset..offset + chunk_len]);

            offset += chunk_len;
        }

        Ok(())
    }

    fn write_bytes_access(
        &mut self,
        vaddr: u64,
        src: &[u8],
        access: AccessType,
    ) -> Result<(), Exception> {
        if src.is_empty() {
            return Ok(());
        }

        self.write_chunks.clear();
        let mut offset = 0usize;
        while offset < src.len() {
            let addr = vaddr
                .checked_add(offset as u64)
                .ok_or(Exception::MemoryFault)?;
            let paddr = self.translate(addr, access)?;

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(src.len() - offset);

            self.write_chunks.push((paddr, chunk_len, offset));
            offset += chunk_len;
        }

        for (paddr, len, src_off) in self.write_chunks.iter().copied() {
            self.platform
                .memory
                .write_physical(paddr, &src[src_off..src_off + len]);
        }

        Ok(())
    }
}

struct WriteIntent<'a> {
    bus: &'a mut PcCpuBus,
}

impl CpuBus for WriteIntent<'_> {
    fn sync(&mut self, state: &aero_cpu_core::state::CpuState) {
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
        self.bus
            .read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut buf = [0u8; 4];
        self.bus
            .read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut buf = [0u8; 8];
        self.bus
            .read_bytes_access(vaddr, &mut buf, AccessType::Write)?;
        Ok(u64::from_le_bytes(buf))
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

impl CpuBus for PcCpuBus {
    fn sync(&mut self, state: &aero_cpu_core::state::CpuState) {
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

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        self.read_bytes_access(vaddr, &mut buf[..len], AccessType::Execute)?;
        Ok(buf)
    }

    fn atomic_rmw<T, R>(&mut self, addr: u64, f: impl FnOnce(T) -> (T, R)) -> Result<R, Exception>
    where
        T: aero_cpu_core::mem::CpuBusValue,
        Self: Sized,
    {
        // Perform the read with write-intent translation so permission checks and A/D-bit
        // updates match real x86 RMW semantics even if the update becomes a no-op.
        let old = {
            let mut intent = WriteIntent { bus: self };
            T::read_from(&mut intent, addr)?
        };
        let (new, ret) = f(old);
        if new != old {
            let mut intent = WriteIntent { bus: self };
            T::write_to(&mut intent, addr, new)?;
        }
        Ok(ret)
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        self.read_bytes_access(vaddr, dst, AccessType::Read)
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        self.write_bytes_access(vaddr, src, AccessType::Write)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        match size {
            1 | 2 | 4 => Ok(self.platform.io.read(port, size as u8) as u64),
            _ => Err(Exception::InvalidOpcode),
        }
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        match size {
            1 | 2 | 4 => {
                self.platform.io.write(port, size as u8, val as u32);
                Ok(())
            }
            _ => Err(Exception::InvalidOpcode),
        }
    }
}
