//! Tier-1 (baseline) JIT pipeline: block discovery + x86→IR translation + WASM codegen.
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

pub mod block {
    pub use crate::block::*;
}

pub mod translate {
    pub use crate::translate::*;
}

pub mod ir {
    pub use crate::tier1_ir::*;
}

pub mod wasm {
    pub use crate::wasm::tier1::*;
}

pub mod pipeline {
    pub use crate::tier1_pipeline::*;
}

pub use crate::block::{discover_block, BasicBlock, BlockEndKind, BlockLimits};
pub use crate::translate::translate_block;
