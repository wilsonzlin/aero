use crate::{PcPlatform, PciIoBarHandler, PciIoBarRouter, SharedPciIoBarMap};
use aero_cpu_core::interrupts::InterruptController as CpuInterruptController;
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::Exception;
use aero_devices::pci::PciBdf;
use aero_mmu::{AccessType, Mmu, TranslateFault};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, SharedPlatformInterrupts,
};

const PAGE_SIZE: u64 = 4096;

struct PciIoBarFromPortIoDevice {
    key: (PciBdf, u8),
    handlers: SharedPciIoBarMap,
}

impl PciIoBarFromPortIoDevice {
    fn read_all_ones(size: usize) -> u32 {
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl PciIoBarHandler for PciIoBarFromPortIoDevice {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        let size_u8 = match size {
            1 | 2 | 4 => size as u8,
            _ => return Self::read_all_ones(size),
        };
        let Ok(offset_u16) = u16::try_from(offset) else {
            return Self::read_all_ones(size);
        };

        let mut handlers = self.handlers.borrow_mut();
        let Some(handler) = handlers.get_mut(&self.key) else {
            return Self::read_all_ones(size);
        };
        handler.read(offset_u16, size_u8)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        let size_u8 = match size {
            1 | 2 | 4 => size as u8,
            _ => return,
        };
        let Ok(offset_u16) = u16::try_from(offset) else {
            return;
        };

        let mut handlers = self.handlers.borrow_mut();
        let Some(handler) = handlers.get_mut(&self.key) else {
            return;
        };
        handler.write(offset_u16, size_u8, value);
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
    pci_io_router: PciIoBarRouter,
}

impl PcCpuBus {
    pub fn new(platform: PcPlatform) -> Self {
        let mut pci_io_router = PciIoBarRouter::new(platform.pci_cfg.clone());

        // Register all PCI I/O BAR handlers defined by the platform. Sort for deterministic
        // dispatch order when devices overlap (misconfigured guests).
        let pci_io_bars = platform.pci_io_bars.clone();
        let mut keys: Vec<(PciBdf, u8)> = pci_io_bars.borrow().keys().copied().collect();
        keys.sort();
        for (bdf, bar) in keys {
            pci_io_router.register_handler(
                bdf,
                bar,
                PciIoBarFromPortIoDevice {
                    key: (bdf, bar),
                    handlers: pci_io_bars.clone(),
                },
            );
        }

        Self {
            platform,
            mmu: Mmu::new(),
            cpl: 0,
            write_chunks: Vec::new(),
            pci_io_router,
        }
    }

    /// Reset CPU-bus-side state back to the initial power-on baseline.
    ///
    /// This is intended for machine reset flows that keep the underlying [`PcPlatform`] instance
    /// (and its device backends) intact, but need to flush paging/MMU caches and privilege state.
    pub fn reset(&mut self) {
        self.mmu = Mmu::new();
        self.cpl = 0;
        self.write_chunks.clear();
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
        match self
            .mmu
            .translate(&mut self.platform.memory, vaddr, access, self.cpl)
        {
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
            let addr = vaddr.wrapping_add(offset as u64);
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
            let addr = vaddr.wrapping_add(offset as u64);
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

    fn preflight_range(
        &mut self,
        vaddr: u64,
        len: usize,
        access: AccessType,
    ) -> Result<(), Exception> {
        if len == 0 {
            return Ok(());
        }

        let mut offset = 0usize;
        while offset < len {
            let addr = vaddr.wrapping_add(offset as u64);
            let _paddr = self.translate(addr, access)?;

            let page_off = (addr & (PAGE_SIZE - 1)) as usize;
            let page_rem = (PAGE_SIZE as usize) - page_off;
            let chunk_len = page_rem.min(len - offset);
            offset += chunk_len;
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

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.preflight_range(vaddr, len, AccessType::Write)
    }

    fn supports_bulk_copy(&self) -> bool {
        // Bulk string fast paths assume contiguous linear addresses map to contiguous physical
        // addresses. When the chipset A20 gate is disabled, physical bit 20 is forced low and the
        // address space aliases in 1MiB windows, which can invalidate those assumptions.
        //
        // Expose bulk ops only once A20 is enabled so Tier-0 won't attempt the fast path during
        // early real-mode boot sequences.
        self.platform.chipset.a20().enabled()
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }

        let len_u64 = len as u64;
        let src_end = src.checked_add(len_u64).ok_or(Exception::MemoryFault)?;
        let dst_end = dst.checked_add(len_u64).ok_or(Exception::MemoryFault)?;

        let overlap = src < dst_end && dst < src_end;
        let copy_backward = overlap && dst > src;

        // Preflight translation so this operation is atomic w.r.t page faults (Tier-0 assumes
        // bulk ops don't partially commit on failure).
        self.preflight_range(src, len, AccessType::Read)?;
        self.preflight_range(dst, len, AccessType::Write)?;

        const BUF_SIZE: usize = 4096;
        let mut buf = [0u8; BUF_SIZE];

        if copy_backward {
            let mut remaining = len;
            while remaining != 0 {
                let chunk_len = remaining.min(BUF_SIZE);
                let start = remaining - chunk_len;
                let src_addr = src + start as u64;
                let dst_addr = dst + start as u64;

                self.read_bytes_access(src_addr, &mut buf[..chunk_len], AccessType::Read)?;
                self.write_bytes_access(dst_addr, &buf[..chunk_len], AccessType::Write)?;

                remaining = start;
            }
        } else {
            let mut offset = 0usize;
            while offset < len {
                let chunk_len = (len - offset).min(BUF_SIZE);
                let src_addr = src + offset as u64;
                let dst_addr = dst + offset as u64;

                self.read_bytes_access(src_addr, &mut buf[..chunk_len], AccessType::Read)?;
                self.write_bytes_access(dst_addr, &buf[..chunk_len], AccessType::Write)?;

                offset += chunk_len;
            }
        }

        Ok(true)
    }

    fn supports_bulk_set(&self) -> bool {
        self.platform.chipset.a20().enabled()
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 || pattern.is_empty() {
            return Ok(true);
        }

        let total = pattern
            .len()
            .checked_mul(repeat)
            .ok_or(Exception::MemoryFault)?;

        // Preflight translation so this operation is atomic w.r.t page faults.
        self.preflight_range(dst, total, AccessType::Write)?;

        const BUF_SIZE: usize = 4096;
        let mut buf = [0u8; BUF_SIZE];

        // Fast-path: if the chunk size is a multiple of the pattern length, each
        // chunk begins at pattern offset 0. We can fill the scratch buffer once
        // and reuse it for all chunks.
        if BUF_SIZE.is_multiple_of(pattern.len()) {
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = pattern[i % pattern.len()];
            }

            let mut offset = 0usize;
            while offset < total {
                let chunk_len = (total - offset).min(BUF_SIZE);
                let dst_addr = dst + offset as u64;
                self.write_bytes_access(dst_addr, &buf[..chunk_len], AccessType::Write)?;
                offset += chunk_len;
            }

            return Ok(true);
        }

        let mut offset = 0usize;
        while offset < total {
            let chunk_len = (total - offset).min(BUF_SIZE);
            let dst_addr = dst + offset as u64;
            // Fill `buf[..chunk_len]` with the correct pattern sequence for this chunk.
            //
            // Note: `bulk_set` is defined as repeating the entire `pattern` byte
            // sequence `repeat` times. The chunk size (BUF_SIZE) is not
            // necessarily a multiple of `pattern.len()`, so we must preserve the
            // pattern offset across chunks.
            let mut pos = 0usize;
            let mut pat_idx = offset % pattern.len();
            while pos < chunk_len {
                let take = (chunk_len - pos).min(pattern.len() - pat_idx);
                buf[pos..pos + take].copy_from_slice(&pattern[pat_idx..pat_idx + take]);
                pos += take;
                pat_idx = 0;
            }

            self.write_bytes_access(dst_addr, &buf[..chunk_len], AccessType::Write)?;
            offset += chunk_len;
        }

        Ok(true)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        let size_usize = match size {
            1 | 2 | 4 => size as usize,
            _ => return Err(Exception::InvalidOpcode),
        };

        if let Some(v) = self.pci_io_router.dispatch_read(port, size_usize) {
            return Ok(u64::from(v));
        }

        Ok(self.platform.io.read(port, size as u8) as u64)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        let size_usize = match size {
            1 | 2 | 4 => size as usize,
            _ => return Err(Exception::InvalidOpcode),
        };

        if self
            .pci_io_router
            .dispatch_write(port, size_usize, val as u32)
        {
            return Ok(());
        }

        self.platform.io.write(port, size as u8, val as u32);
        Ok(())
    }
}
