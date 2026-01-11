use crate::address_filter::AddressFilter;
use crate::chipset::A20GateHandle;
use memory::{DenseMemory, GuestMemory, MapError, MmioHandler, PhysicalMemoryBus};
use std::sync::Arc;

/// Base address of the system BIOS ROM in the 20-bit real-mode memory window.
pub const BIOS_ROM_BASE: u64 = 0x000F_0000;
/// Size of the system BIOS ROM mapping (64 KiB).
pub const BIOS_ROM_SIZE: usize = 0x10000;
/// Reset-vector alias of the BIOS ROM at the top of the 32-bit physical address space.
pub const BIOS_ROM_ALIAS_BASE: u64 = 0xFFFF_0000;
/// Architectural reset vector physical address (16 bytes below 4 GiB).
pub const BIOS_RESET_VECTOR_PHYS: u64 = 0xFFFF_FFF0;

const A20_BIT: u64 = 1 << 20;
const A20_BOUNDARY_MASK: u64 = A20_BIT - 1;

/// PC physical memory bus (RAM + ROM + MMIO) with chipset-level address filtering.
///
/// This is the canonical physical address router for the PC platform:
/// - RAM is backed by a [`memory::GuestMemory`] implementation.
/// - ROM is read-only and may be mapped at multiple aliases (e.g. BIOS).
/// - MMIO takes precedence over ROM and RAM.
/// - A20 gating is applied to *all* physical accesses when disabled.
pub struct MemoryBus {
    filter: AddressFilter,
    bus: PhysicalMemoryBus,
}

impl MemoryBus {
    pub fn new(filter: AddressFilter, ram_size: usize) -> Self {
        let ram = DenseMemory::new(ram_size as u64).expect("failed to allocate guest RAM");
        Self::with_ram(filter, Box::new(ram))
    }

    pub fn with_ram(filter: AddressFilter, ram: Box<dyn GuestMemory>) -> Self {
        Self {
            filter,
            bus: PhysicalMemoryBus::new(ram),
        }
    }

    pub fn a20(&self) -> A20GateHandle {
        self.filter.a20()
    }

    pub fn ram(&self) -> &dyn GuestMemory {
        &*self.bus.ram
    }

    pub fn ram_mut(&mut self) -> &mut dyn GuestMemory {
        &mut *self.bus.ram
    }

    pub fn map_rom(&mut self, start: u64, data: Arc<[u8]>) -> Result<(), MapError> {
        self.bus.map_rom(start, data)
    }

    pub fn map_mmio(
        &mut self,
        start: u64,
        len: u64,
        handler: Box<dyn MmioHandler>,
    ) -> Result<(), MapError> {
        self.bus.map_mmio(start, len, handler)
    }

    /// Map the system BIOS ROM into the conventional `F0000..=FFFFF` legacy window and the
    /// top-of-4GiB reset-vector alias `FFFF_0000..=FFFF_FFFF`.
    ///
    /// The BIOS ROM image is expected to be exactly 64 KiB (matching the `F000` segment).
    pub fn map_system_bios_rom(&mut self, rom: Arc<[u8]>) -> Result<(), MapError> {
        assert_eq!(
            rom.len(),
            BIOS_ROM_SIZE,
            "system BIOS ROM must be {BIOS_ROM_SIZE} bytes (got {})",
            rom.len()
        );

        self.map_rom(BIOS_ROM_BASE, Arc::clone(&rom))?;
        self.map_rom(BIOS_ROM_ALIAS_BASE, rom)?;
        Ok(())
    }

    pub fn read_u8(&mut self, paddr: u64) -> u8 {
        let mut buf = [0u8; 1];
        self.read_physical(paddr, &mut buf);
        buf[0]
    }

    pub fn write_u8(&mut self, paddr: u64, value: u8) {
        self.write_physical(paddr, &[value]);
    }

    pub fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.read_physical_impl(paddr, buf);
    }

    pub fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.write_physical_impl(paddr, buf);
    }

    fn read_physical_impl(&mut self, paddr: u64, dst: &mut [u8]) {
        if dst.is_empty() {
            return;
        }

        if self.filter.a20().enabled() {
            self.bus.read_physical(paddr, dst);
            return;
        }

        let mut pos = 0usize;
        while pos < dst.len() {
            let Some(addr) = paddr.checked_add(pos as u64) else {
                dst[pos..].fill(0xFF);
                break;
            };

            // Split on 1MiB boundaries because A20 masking changes bit 20 (the 1MiB bit), so
            // a linear physical access may become non-contiguous after masking.
            let next_boundary = (addr | A20_BOUNDARY_MASK)
                .checked_add(1)
                .unwrap_or(u64::MAX);
            let max_len = next_boundary.saturating_sub(addr);
            let remaining = (dst.len() - pos) as u64;

            let mut chunk_len = std::cmp::min(max_len, remaining) as usize;
            if chunk_len == 0 {
                chunk_len = dst.len() - pos;
            }

            let filtered = addr & !A20_BIT;
            self.bus
                .read_physical(filtered, &mut dst[pos..pos + chunk_len]);
            pos += chunk_len;
        }
    }

    fn write_physical_impl(&mut self, paddr: u64, src: &[u8]) {
        if src.is_empty() {
            return;
        }

        if self.filter.a20().enabled() {
            self.bus.write_physical(paddr, src);
            return;
        }

        let mut pos = 0usize;
        while pos < src.len() {
            let Some(addr) = paddr.checked_add(pos as u64) else {
                break;
            };

            let next_boundary = (addr | A20_BOUNDARY_MASK)
                .checked_add(1)
                .unwrap_or(u64::MAX);
            let max_len = next_boundary.saturating_sub(addr);
            let remaining = (src.len() - pos) as u64;

            let mut chunk_len = std::cmp::min(max_len, remaining) as usize;
            if chunk_len == 0 {
                chunk_len = src.len() - pos;
            }

            let filtered = addr & !A20_BIT;
            self.bus
                .write_physical(filtered, &src[pos..pos + chunk_len]);
            pos += chunk_len;
        }
    }
}

impl memory::MemoryBus for MemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.read_physical_impl(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.write_physical_impl(paddr, buf);
    }
}
