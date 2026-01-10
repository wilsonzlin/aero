//! Compatibility shims for legacy traits in `crates/memory`.
//!
//! The project currently has multiple memory abstractions. `aero-mem` is the
//! thread-safe / WASM-friendly implementation, while `crates/memory` contains
//! older traits used by some subsystems (MMU page table walkers, etc).
//!
//! Enabling the `memory-compat` feature lets `aero-mem` types implement those
//! legacy traits without introducing an unconditional dependency.

use crate::{MemoryBus, PhysicalMemory, PhysicalMemoryError};

fn map_phys_err(err: PhysicalMemoryError) -> memory::GuestMemoryError {
    match err {
        PhysicalMemoryError::InvalidChunkSize { chunk_size } => {
            memory::GuestMemoryError::InvalidChunkSize { chunk_size }
        }
        PhysicalMemoryError::TooLarge { size, .. } => {
            memory::GuestMemoryError::SizeTooLarge { size }
        }
        PhysicalMemoryError::OutOfBounds { addr, len, size } => {
            memory::GuestMemoryError::OutOfRange {
                paddr: addr,
                len,
                size,
            }
        }
    }
}

impl memory::GuestMemory for PhysicalMemory {
    fn size(&self) -> u64 {
        self.len()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> memory::GuestMemoryResult<()> {
        self.try_read_bytes(paddr, dst).map_err(map_phys_err)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> memory::GuestMemoryResult<()> {
        self.try_write_bytes(paddr, src).map_err(map_phys_err)
    }
}

impl memory::bus::MemoryBus for MemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        // Reading unmapped physical addresses commonly returns 0xFF on real
        // hardware. Start with that and overwrite bytes we can resolve.
        buf.fill(0xFF);
        let _ = self.try_read_bytes(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        // Writes to unmapped space are ignored.
        let _ = self.try_write_bytes(paddr, buf);
    }
}
