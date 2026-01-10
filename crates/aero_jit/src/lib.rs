mod cpu;
pub mod interp;
pub mod ir;
pub mod opt;
pub mod profile;
pub mod simd;
pub mod t2_exec;
pub mod t2_ir;
pub mod trace;
pub mod wasm;

// Tier-1 front-end (baseline): basic block discovery + x86→IR lowering used by
// unit tests and early JIT bring-up.
pub mod block;
pub mod tier1_ir;
pub mod translate;

pub use block::{discover_block, BasicBlock, BlockEndKind, BlockLimits};
pub use translate::translate_block;
pub use cpu::{
    CpuState, Reg, JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK,
    PAGE_OFFSET_MASK, PAGE_SHIFT, PAGE_SIZE, TLB_FLAG_EXEC, TLB_FLAG_IS_RAM, TLB_FLAG_READ,
    TLB_FLAG_WRITE,
};
