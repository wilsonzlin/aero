//! Tier-1 (baseline) JIT pipeline: block discovery + x86→IR translation + WASM codegen.
//!
//! This tier is responsible for:
//! - basic block discovery (`block`)
//! - lowering decoded x86 instructions to a small IR (`translate`, `ir`)
//! - emitting WASM for a single block (`wasm_codegen`)
//!
//! The intended integration path with [`aero_cpu_core::jit::runtime::JitRuntime`] is:
//!
//! 1. Install a [`pipeline::Tier1CompileQueue`] as the runtime's
//!    [`aero_cpu_core::jit::runtime::CompileRequestSink`].
//! 2. Drain queued RIPs from the host/worker thread.
//! 3. Use [`pipeline::Tier1Compiler`] to compile a Tier-1 block into a
//!    [`aero_cpu_core::jit::cache::CompiledBlockHandle`].
//! 4. Install the handle via [`aero_cpu_core::jit::runtime::JitRuntime::install_handle`].
//!
//! On native targets (`cfg(not(target_arch = "wasm32"))`), [`crate::backend::WasmBackend`]
//! implements both [`crate::Tier1Bus`] and [`pipeline::Tier1WasmRegistry`], making it a convenient
//! starting point for experiments and tests.

pub mod block;
pub mod ir;
pub mod translate;
pub mod wasm_codegen;

pub mod pipeline {
    pub use crate::tier1_pipeline::*;
}

pub use block::{discover_block, discover_block_mode, BasicBlock, BlockEndKind, BlockLimits};
pub use translate::translate_block;
pub use wasm_codegen::{
    Tier1WasmCodegen, Tier1WasmOptions, EXPORT_BLOCK_FN, EXPORT_TIER1_BLOCK_FN,
};
