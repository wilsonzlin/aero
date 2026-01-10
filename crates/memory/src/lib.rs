//! Guest memory backends (dense + sparse).

pub mod bus;
pub mod mmu;
pub mod phys;
pub mod tlb;

pub use bus::{Bus, MemoryBus, MmioHandler};
pub use mmu::{AccessType, Mmu, TranslateError};
pub use phys::{DenseMemory, GuestMemory, GuestMemoryError, GuestMemoryResult, SparseMemory};
pub use tlb::{PageSize, Tlb, TlbEntry};

#[cfg(test)]
mod tests;
