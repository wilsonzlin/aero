use crate::PcPlatform;
use aero_cpu_core::interrupts::InterruptController as CpuInterruptController;
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::Exception;
use aero_mmu::{AccessType, MemoryBus as MmuBus, Mmu, TranslateFault};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, SharedPlatformInterrupts,
};
use memory::MemoryBus as PhysicalMemoryBus;

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
}

impl PcCpuBus {
    pub fn new(platform: PcPlatform) -> Self {
        Self {
            platform,
            mmu: Mmu::new(),
            cpl: 0,
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
