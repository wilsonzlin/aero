//! CPU-side helpers for time-related instructions and feature reporting.

mod cpuid;
mod regs;
mod time_instructions;

pub use cpuid::{CpuidModel, CpuidResult};
pub use regs::CpuRegs;
pub use time_instructions::{Msr, MsrError, TimeInstructions};
