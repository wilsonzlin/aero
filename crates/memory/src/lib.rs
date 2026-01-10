//! Guest memory backends (dense + sparse).

pub mod bus;
pub mod mmu;
pub mod phys;

pub use bus::MemoryBus;
pub use phys::{DenseMemory, GuestMemory, GuestMemoryError, GuestMemoryResult, SparseMemory};
