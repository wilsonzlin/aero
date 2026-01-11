//! Native (non-wasm32) runtime backend for executing dynamically-generated WASM blocks.
//!
//! This module provides a reference implementation that can drive
//! `aero_cpu_core::jit::runtime::JitRuntime` by running Tier-1 blocks produced by
//! [`crate::wasm::tier1::Tier1WasmCodegen`].

use aero_cpu::CpuState;

/// Minimal interface a host CPU type must expose to execute Tier-1 WASM blocks.
///
/// The Tier-1 WASM ABI uses the in-memory layout of [`aero_cpu::CpuState`]. The backend copies
/// this state into the shared `WebAssembly.Memory`, calls the compiled block, and then copies the
/// updated state back into the host CPU value.
pub trait Tier1Cpu {
    fn tier1_state(&self) -> &CpuState;
    fn tier1_state_mut(&mut self) -> &mut CpuState;
}

mod wasmtime;

pub use wasmtime::WasmtimeBackend;

