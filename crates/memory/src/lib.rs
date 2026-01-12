//! Guest physical memory utilities.
//!
//! This crate provides guest RAM backends (`DenseMemory`, `SparseMemory`) as well as a guest
//! *physical* memory bus (`MemoryBus`) used by the MMU for page table walks. The [`bus`] module also
//! contains routing implementations that support RAM/ROM/MMIO.

pub mod bus;
pub mod dirty;
pub mod mapped;
pub mod mmu;
pub mod phys;
pub mod tlb;

pub use bus::{Bus, MapError, MemoryBus, MmioHandler, MmioRegion, PhysicalMemoryBus, RomRegion};
pub use dirty::{DirtyGuestMemory, DirtyTracker};
pub use mapped::{GuestMemoryMapping, MappedGuestMemory, MappedGuestMemoryError};
pub use mmu::{AccessType, Mmu, TranslateError};
pub use phys::{
    DenseMemory, GuestMemory, GuestMemoryError, GuestMemoryResult, MappedRegion, SparseMemory,
};
pub use tlb::{PageSize, Tlb, TlbEntry};

/// Alias preserved for older callers; the MMU returns a [`TranslateError`] which
/// may contain an encoded x86 page fault.
pub type PageFault = TranslateError;

#[cfg(test)]
mod tests;
