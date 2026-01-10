//! Minimal Tier-0 execution engine prototype.
//!
//! This crate implements a small x86-64 *subset* interpreter in two modes:
//! - A legacy single-step interpreter (`LegacyInterpreter`)
//! - A decoded-block cached interpreter with a table-driven dispatch loop
//!   (`Tier0Interpreter`)
//!
//! The goal is to demonstrate the Tier-0 performance layer architecture:
//! decoded-block caching + fast opcode dispatch + page-version invalidation.

pub mod bus;
pub mod cpu;
pub mod decoder;
pub mod dispatch;
pub mod interpreter;

pub use bus::{CpuBus, MemoryBus};
pub use cpu::{CpuState, Reg};
pub use interpreter::{ExitReason, LegacyInterpreter, Tier0Interpreter};
