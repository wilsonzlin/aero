//! Emulator-facing physical memory bus.
//!
//! Historically this crate carried its own RAM/ROM/MMIO router. The project now has a canonical
//! PC physical memory bus implementation in `aero-platform`, backed by `memory::PhysicalMemoryBus`.
//! This module keeps the emulator test helpers working while avoiding a forked bus
//! implementation that could drift over time.

use aero_platform::address_filter::AddressFilter;
use aero_platform::ChipsetState;
use memory::{GuestMemory, MapError, MmioHandler};
use std::sync::Arc;

/// Physical address router used by emulator crate unit tests.
///
/// This wraps [`aero_platform::memory::MemoryBus`] so it automatically inherits:
/// - RAM + ROM + MMIO routing (MMIO > ROM > RAM)
/// - A20 masking when disabled
/// - BIOS ROM alias helpers (via `map_system_bios_rom`)
pub struct MemoryBus {
    inner: aero_platform::memory::MemoryBus,
}

impl MemoryBus {
    /// Create a new bus with the provided guest RAM backend.
    ///
    /// A20 is enabled by default; tests that need wraparound behaviour should explicitly disable
    /// it via `bus.a20().set_enabled(false)`.
    pub fn new(ram: Box<dyn GuestMemory>) -> Self {
        let chipset = ChipsetState::new(true);
        let filter = AddressFilter::new(chipset.a20());
        Self {
            inner: aero_platform::memory::MemoryBus::with_ram(filter, ram),
        }
    }

    pub fn a20(&self) -> aero_platform::A20GateHandle {
        self.inner.a20()
    }

    pub fn ram(&self) -> &dyn GuestMemory {
        self.inner.ram()
    }

    pub fn ram_mut(&mut self) -> &mut dyn GuestMemory {
        self.inner.ram_mut()
    }

    pub fn add_rom_region(&mut self, start: u64, data: Vec<u8>) -> Result<(), MapError> {
        self.inner.map_rom(start, Arc::from(data))
    }

    pub fn add_mmio_region(
        &mut self,
        start: u64,
        len: u64,
        handler: Box<dyn MmioHandler>,
    ) -> Result<(), MapError> {
        self.inner.map_mmio(start, len, handler)
    }

    pub fn map_system_bios_rom(&mut self, rom: Arc<[u8]>) -> Result<(), MapError> {
        self.inner.map_system_bios_rom(rom)
    }
}

impl memory::MemoryBus for MemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        memory::MemoryBus::read_physical(&mut self.inner, paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        memory::MemoryBus::write_physical(&mut self.inner, paddr, buf);
    }
}
