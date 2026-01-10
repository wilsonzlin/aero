#![forbid(unsafe_code)]

pub mod descriptors;
pub mod interrupts;
pub mod msr;
pub mod state;

pub mod tier0;

// Standalone Tier-1 baseline JIT harness.
//
// NOTE: The production CPU implementation lives in `tier0` for now. These
// modules intentionally form an isolated prototype (small x86 subset) used by
// differential tests and microbenchmarks while the full integration into the
// main CPU worker evolves.
pub mod interpreter;
pub mod jit;
pub mod memory;

pub mod baseline {
    pub use super::interpreter::Interpreter;
    pub use super::jit::{CpuWorker, JitConfig};
    pub use super::memory::Memory;
}
