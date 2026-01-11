pub mod abi;
pub mod jit_ctx;

mod block;
pub mod compiler;
mod opt;
mod t2_exec;
mod t2_ir;
mod tier1_bus;
mod tier1_ir;
mod tier1_pipeline;
mod trace;
mod translate;
pub mod profile;
pub mod simd;
pub mod tier1;
pub mod tier2;
mod tier2_builder;
pub mod wasm;

// ---- JIT ABI constants ------------------------------------------------------

/// 4KiB page shift used by the inline JIT TLB fast-path.
pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_OFFSET_MASK: u64 = PAGE_SIZE - 1;
pub const PAGE_BASE_MASK: u64 = !PAGE_OFFSET_MASK;

/// Number of entries in the direct-mapped JIT TLB.
pub const JIT_TLB_ENTRIES: usize = 256;
pub const JIT_TLB_INDEX_MASK: u64 = (JIT_TLB_ENTRIES as u64) - 1;

/// Size of a single TLB entry in bytes (`tag: u64` + `data: u64`).
pub const JIT_TLB_ENTRY_SIZE: u32 = 16;

/// Entry flags packed into the low 12 bits of the returned translation `data` word.
pub const TLB_FLAG_READ: u64 = 1 << 0;
pub const TLB_FLAG_WRITE: u64 = 1 << 1;
pub const TLB_FLAG_EXEC: u64 = 1 << 2;
pub const TLB_FLAG_IS_RAM: u64 = 1 << 3;

// Native (non-wasm32) JIT backend glue for `aero_cpu_core::jit::runtime::JitRuntime`.
#[cfg(not(target_arch = "wasm32"))]
pub mod backend;

pub use tier1_bus::Tier1Bus;
