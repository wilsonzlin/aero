//! Guest memory backends (dense + sparse).

pub mod phys;

pub use phys::{DenseMemory, GuestMemory, GuestMemoryError, GuestMemoryResult, SparseMemory};
