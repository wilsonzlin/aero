//! `aero-jit-proto` is a self-contained prototype of Aero's tiered JIT pipeline.
//!
//! This crate intentionally does **not** attempt to emulate x86. Instead, it
//! provides a small "guest ISA" plus tiered execution (interpreter → tier1 →
//! tier2) to encode the architectural requirements of Aero's Tier-2 optimizing
//! JIT:
//!
//! - Profile collection (per-block counts, branch profiling, call graph).
//! - Hot trace/region selection and compilation.
//! - Optimization passes (const fold, DCE, CSE, flag liveness, strength
//!   reduction, LICM, and a WASM-local-style register allocation).
//! - Deoptimization guards (self-modifying code / page permission epoch).
//! - Differential testing harness vs interpreter.
//! - Microbench binary (`cargo run -p aero-jit-proto --bin microbench`).
//!
//! The intent is to serve as an executable specification for the Tier-2 design
//! described in `docs/10-performance-optimization.md`.

pub mod microvm;
mod opt;
mod profile;
mod tier;

pub use microvm::{Cond, FuncId, Gpr, Program, Vm};
pub use tier::{Engine, EngineStats, JitConfig};

// -------------------------------------------------------------------------------------------------
// Tier-1 baseline x86 pipeline (block discovery → IR → WASM emission).
// -------------------------------------------------------------------------------------------------
//
// This is intentionally minimal and is expected to grow toward the Tier-1 JIT
// described in `docs/02-cpu-emulation.md`.
pub mod block;
pub mod cpu;
pub mod ir;
pub mod wasm;
pub mod x86;
