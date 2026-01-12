use crate::address_filter::AddressFilter;
use crate::chipset::A20GateHandle;
use crate::dirty_memory::{DirtyTrackingHandle, DirtyTrackingMemory, DEFAULT_DIRTY_PAGE_SIZE};
use aero_pc_constants::PCIE_ECAM_BASE;
use memory::{
    DenseMemory, GuestMemory, GuestMemoryMapping, MapError, MappedGuestMemory, MmioHandler,
    PhysicalMemoryBus,
};
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
    dirty: Option<DirtyTrackingHandle>,
}

impl MemoryBus {
    /// Wrap a contiguous RAM backend with the PC high-memory layout when RAM exceeds the PCIe ECAM
    /// base.
    ///
    /// When RAM is larger than [`PCIE_ECAM_BASE`], firmware reserves the physical range
    /// `PCIE_ECAM_BASE..4GiB` for PCIe ECAM and other MMIO windows, and remaps the RAM that would
    /// have occupied that hole to start at 4GiB.
    fn wrap_pc_high_memory(ram: Box<dyn GuestMemory>) -> Box<dyn GuestMemory> {
        let ram_bytes = ram.size();
        if ram_bytes <= PCIE_ECAM_BASE {
            return ram;
        }

        const HIGH_RAM_BASE: u64 = 0x1_0000_0000;
        let high_len = ram_bytes - PCIE_ECAM_BASE;
        let phys_size = HIGH_RAM_BASE
            .checked_add(high_len)
            .expect("high RAM end overflow");

        // Leave the guest-physical `[PCIE_ECAM_BASE, HIGH_RAM_BASE)` range unmapped so reads behave
        // as open bus (0xFF) and writes are ignored, matching PC PCI hole semantics.
        let mapped = MappedGuestMemory::new(
            ram,
            phys_size,
            vec![
                // Low RAM: [0..PCIE_ECAM_BASE)
                GuestMemoryMapping {
                    phys_start: 0,
                    phys_end: PCIE_ECAM_BASE,
                    inner_offset: 0,
                },
                // High RAM: [4GiB..4GiB + (ram_bytes - PCIE_ECAM_BASE))
                GuestMemoryMapping {
                    phys_start: HIGH_RAM_BASE,
                    phys_end: phys_size,
                    inner_offset: PCIE_ECAM_BASE,
                },
            ],
        )
        .expect("valid PC RAM remapping layout");
        Box::new(mapped)
    }

    pub fn new(filter: AddressFilter, ram_size: usize) -> Self {
        let ram = DenseMemory::new(ram_size as u64).expect("failed to allocate guest RAM");
        Self::with_ram(filter, Box::new(ram))
    }

    pub fn with_ram(filter: AddressFilter, ram: Box<dyn GuestMemory>) -> Self {
        let ram = Self::wrap_pc_high_memory(ram);
        Self {
            filter,
            bus: PhysicalMemoryBus::new(ram),
            dirty: None,
        }
    }

    /// Construct a memory bus backed by guest RAM wrapped in dirty-page tracking.
    ///
    /// This is suitable for `aero_snapshot::RamMode::Dirty`: all writes that reach guest RAM via
    /// [`MemoryBus::write_physical`] (including device/DMA writes) are tracked.
    pub fn new_with_dirty_tracking(filter: AddressFilter, ram_size: usize, page_size: u32) -> Self {
        let ram = DenseMemory::new(ram_size as u64).expect("failed to allocate guest RAM");
        Self::with_ram_dirty_tracking(filter, Box::new(ram), page_size)
    }

    /// Like [`MemoryBus::with_ram`], but wraps the provided RAM backend in dirty-page tracking.
    pub fn with_ram_dirty_tracking(
        filter: AddressFilter,
        ram: Box<dyn GuestMemory>,
        page_size: u32,
    ) -> Self {
        // Important: dirty tracking must wrap the backing memory *before* we apply any guest
        // physical remapping so dirty page indices remain in `[0..ram_bytes)`.
        let dirty_ram = DirtyTrackingMemory::new(ram, page_size);
        let handle = dirty_ram.tracking_handle();
        let ram = Self::wrap_pc_high_memory(Box::new(dirty_ram));
        Self {
            filter,
            bus: PhysicalMemoryBus::new(ram),
            dirty: Some(handle),
        }
    }

    pub fn a20(&self) -> A20GateHandle {
        self.filter.a20()
    }

    /// Returns the underlying guest RAM backend.
    ///
    /// This bypasses all chipset-level address filtering (including A20 masking) because it
    /// accesses the raw [`memory::GuestMemory`] implementation directly.
    ///
    /// Use this for host-side operations that must observe *true* physical RAM contents (e.g.
    /// snapshot save/restore). Guest-visible physical accesses should go through
    /// [`MemoryBus::read_physical`] / [`MemoryBus::write_physical`] so they correctly model the
    /// platform's address filtering behavior.
    pub fn ram(&self) -> &dyn GuestMemory {
        &*self.bus.ram
    }

    /// Mutable access to the underlying guest RAM backend.
    ///
    /// Like [`MemoryBus::ram`], this bypasses chipset-level address filtering (A20, etc).
    pub fn ram_mut(&mut self) -> &mut dyn GuestMemory {
        &mut *self.bus.ram
    }

    /// Dirty page size (in bytes) used by [`MemoryBus::take_dirty_pages`], if enabled.
    ///
    /// Defaults to 4096 for snapshot compatibility.
    pub fn dirty_page_size(&self) -> u32 {
        self.dirty
            .as_ref()
            .map(|h| h.page_size())
            .unwrap_or(DEFAULT_DIRTY_PAGE_SIZE)
    }

    /// Return and clear the set of guest RAM pages dirtied since the last call.
    ///
    /// Returns `None` when dirty tracking is not enabled for this bus.
    pub fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        self.dirty.as_ref().map(|h| h.take_dirty_pages())
    }

    /// Clear all dirty page tracking state (if enabled).
    pub fn clear_dirty(&mut self) {
        if let Some(handle) = &self.dirty {
            handle.clear_dirty();
        }
    }

    pub fn map_rom(&mut self, start: u64, data: Arc<[u8]>) -> Result<(), MapError> {
        let len = data.len();
        match self.bus.map_rom(start, data) {
            Ok(()) => Ok(()),
            Err(MapError::Overlap) => {
                // BIOS resets may re-map the same ROM windows. Treat identical overlaps as
                // idempotent, but reject unexpected overlaps to avoid silently corrupting the bus.
                let already_mapped = self
                    .bus
                    .rom_regions()
                    .iter()
                    .any(|r| r.start == start && r.data.len() == len);
                if already_mapped {
                    Ok(())
                } else {
                    Err(MapError::Overlap)
                }
            }
            Err(MapError::AddressOverflow) => Err(MapError::AddressOverflow),
        }
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
            let next_boundary = (addr | A20_BOUNDARY_MASK).saturating_add(1);
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

            let next_boundary = (addr | A20_BOUNDARY_MASK).saturating_add(1);
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

impl aero_mmu::MemoryBus for MemoryBus {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        memory::MemoryBus::read_u16(self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        memory::MemoryBus::read_u32(self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        memory::MemoryBus::read_u64(self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        memory::MemoryBus::write_u16(self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        memory::MemoryBus::write_u32(self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        memory::MemoryBus::write_u64(self, paddr, value)
    }
}
