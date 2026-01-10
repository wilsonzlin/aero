//! Tier-0 x86/x86-64 interpreter.
//!
//! This module prioritizes correctness over performance and serves as the reference interpreter
//! for higher-tier JITs.

#![forbid(unsafe_code)]

mod bus;
mod cpu;
mod error;
mod exec;
mod flags;

pub use bus::{MemoryBus, PortIo};
pub use cpu::{CpuMode, CpuState, Segment};
pub use error::EmuException;
pub use exec::{Machine, StepOutcome};
pub use flags::{Flag, FLAGS_ARITH_MASK};
