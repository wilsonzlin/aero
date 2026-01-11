pub mod abi;
mod cpu;
pub mod interp;
pub mod ir;
pub mod opt;
pub mod profile;
pub mod simd;
pub mod t2_exec;
pub mod t2_ir;
pub mod tier2;
pub mod trace;
pub mod wasm;
pub mod compiler;

// Tier-1 front-end (baseline): basic block discovery + x86→IR lowering used by
// unit tests and early JIT bring-up.
pub mod block;
pub mod tier1_bus;
pub mod tier1_pipeline;
pub mod tier1_ir;
pub mod translate;

// Native (non-wasm32) JIT backend glue for `aero_cpu_core::jit::runtime::JitRuntime`.
#[cfg(not(target_arch = "wasm32"))]
pub mod backend;

pub use block::{discover_block, BasicBlock, BlockEndKind, BlockLimits};
pub use tier1_bus::Tier1Bus;
pub use tier1_pipeline::{
    CodeProvider, Tier1CompileError, Tier1CompileQueue, Tier1Compiler, Tier1WasmRegistry,
};
pub use cpu::{
    CpuState, Reg, JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK,
    PAGE_OFFSET_MASK, PAGE_SHIFT, PAGE_SIZE, TLB_FLAG_EXEC, TLB_FLAG_IS_RAM, TLB_FLAG_READ,
    TLB_FLAG_WRITE,
};
pub use translate::translate_block;
