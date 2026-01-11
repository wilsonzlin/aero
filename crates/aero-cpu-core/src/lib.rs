#![forbid(unsafe_code)]

//! Core architectural CPU state + Tier-0 interpreter + JIT runtime for Aero.
//!
//! The crate API is intentionally centered around [`state::CpuState`], which is
//! the stable in-memory ABI shared by:
//! - the Tier-0 interpreter (`interp::tier0`), used for cold code and testing
//! - the JIT runtime (`jit`), which executes dynamically generated WASM blocks
//!
//! The older `cpu.rs`/`bus.rs` interpreter stack is kept behind the
//! `legacy-interp` Cargo feature (default-off).

mod exception;
mod fxsave;

pub mod assist;
pub mod cpuid;
pub mod descriptors;
pub mod exceptions;
pub mod exec;
pub mod fpu;
pub mod interp;
pub mod interrupts;
pub mod jit;
pub mod mem;
pub mod mode;
pub mod msr;
pub mod paging_bus;
pub mod segmentation;
pub mod sse_state;
pub mod state;
pub mod system;
pub mod time;
pub mod time_insn;

#[cfg(feature = "legacy-interp")]
pub mod bus;
#[cfg(feature = "legacy-interp")]
pub mod cpu;

pub use exception::{AssistReason, Exception};
pub use mem::CpuBus;
pub use paging_bus::PagingBus;
pub use state::CpuState;

#[cfg(feature = "legacy-interp")]
pub use bus::{Bus, RamBus};
#[cfg(feature = "legacy-interp")]
pub use cpu::{Cpu, CpuMode, Segment};

/// The architectural size of the FXSAVE/FXRSTOR memory image.
pub const FXSAVE_AREA_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxStateError {
    /// Attempted to load an MXCSR value with reserved bits set.
    ///
    /// On real hardware this would raise a #GP(0).
    MxcsrReservedBits { value: u32, mask: u32 },
}

impl From<FxStateError> for Exception {
    fn from(_value: FxStateError) -> Self {
        // Both `LDMXCSR` and `FXRSTOR` raise #GP(0) when MXCSR has reserved bits set.
        Exception::gp0()
    }
}
