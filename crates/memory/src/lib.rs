//! Guest memory backends (dense + sparse).

pub mod bus;
pub mod mmu;
pub mod phys;
pub mod tlb;

pub use bus::{Bus, MemoryBus, MmioHandler};
pub use mmu::{AccessType, Mmu, TranslateError};
pub use phys::{DenseMemory, GuestMemory, GuestMemoryError, GuestMemoryResult, SparseMemory};
pub use tlb::{PageSize, Tlb, TlbEntry};

/// Alias preserved for older callers; the MMU returns a [`TranslateError`] which
/// may contain an encoded x86 page fault.
pub type PageFault = TranslateError;

#[cfg(test)]
mod tests;
