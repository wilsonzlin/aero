//! Aero's JIT compiler pipelines.
//!
//! This crate is split into explicit tiers to avoid pipeline ambiguity:
//! - [`tier1`]: basic block discovery, x86 → Tier-1 IR lowering, and single-block WASM codegen.
//! - [`tier2`]: trace IR, optimizer passes, trace builder, and trace WASM codegen.
//! - [`legacy`]: the old baseline pipeline (feature-gated behind `legacy-baseline`).
//!
//! The default build enables Tier-1 + Tier-2 only. Enable the `legacy-baseline` feature to access
//! the legacy baseline ABI (`CpuState`/`Reg`) and baseline WASM codegen.

pub mod abi;
pub mod compiler;
pub mod jit_ctx;
pub mod simd;
pub mod tier1;
pub mod tier1_pipeline;
pub mod tier2;
pub mod wasm;

// Native (non-wasm32) JIT backend glue for `aero_cpu_core::jit::runtime::JitRuntime`.
#[cfg(not(target_arch = "wasm32"))]
pub mod backend;

mod tier1_bus;

pub use tier1_bus::Tier1Bus;

#[cfg(feature = "legacy-baseline")]
pub mod legacy;

// ---- Shared JIT constants ---------------------------------------------------------------------

/// 4KiB page shift used by the JIT TLB.
pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_OFFSET_MASK: u64 = PAGE_SIZE - 1;
pub const PAGE_BASE_MASK: u64 = !PAGE_OFFSET_MASK;

/// Number of entries in the direct-mapped JIT TLB.
pub const JIT_TLB_ENTRIES: usize = 256;
pub const JIT_TLB_INDEX_MASK: u64 = (JIT_TLB_ENTRIES as u64) - 1;

/// Size of a single TLB entry in bytes (`tag: u64` + `data: u64`).
pub const JIT_TLB_ENTRY_SIZE: u32 = 16;

const _: () = {
    // The JIT TLB is indexed either with a mask or modulo; keep the mask path valid.
    assert!(JIT_TLB_ENTRIES > 0);
    assert!(JIT_TLB_ENTRIES.is_power_of_two());
    assert!(JIT_TLB_INDEX_MASK == (JIT_TLB_ENTRIES as u64) - 1);

    // The inline TLB layout is `{ tag: u64, data: u64 }`.
    assert!(JIT_TLB_ENTRY_SIZE as usize == core::mem::size_of::<[u64; 2]>());
};

/// Entry flags packed into the low 12 bits of the translation `data` word.
pub const TLB_FLAG_READ: u64 = 1 << 0;
pub const TLB_FLAG_WRITE: u64 = 1 << 1;
pub const TLB_FLAG_EXEC: u64 = 1 << 2;
pub const TLB_FLAG_IS_RAM: u64 = 1 << 3;

const _: () = {
    // Flags are packed into the low 12 bits of the translation `data` word.
    const FLAGS_LIMIT: u64 = PAGE_SIZE;

    assert!(TLB_FLAG_READ < FLAGS_LIMIT);
    assert!(TLB_FLAG_WRITE < FLAGS_LIMIT);
    assert!(TLB_FLAG_EXEC < FLAGS_LIMIT);
    assert!(TLB_FLAG_IS_RAM < FLAGS_LIMIT);

    // Each flag must be a single bit.
    assert!(TLB_FLAG_READ.is_power_of_two());
    assert!(TLB_FLAG_WRITE.is_power_of_two());
    assert!(TLB_FLAG_EXEC.is_power_of_two());
    assert!(TLB_FLAG_IS_RAM.is_power_of_two());

    // Ensure no overlap.
    assert!((TLB_FLAG_READ & TLB_FLAG_WRITE) == 0);
    assert!((TLB_FLAG_READ & TLB_FLAG_EXEC) == 0);
    assert!((TLB_FLAG_READ & TLB_FLAG_IS_RAM) == 0);
    assert!((TLB_FLAG_WRITE & TLB_FLAG_EXEC) == 0);
    assert!((TLB_FLAG_WRITE & TLB_FLAG_IS_RAM) == 0);
    assert!((TLB_FLAG_EXEC & TLB_FLAG_IS_RAM) == 0);

    // JS glue exports these flags as u32 values.
    assert!(TLB_FLAG_READ <= u32::MAX as u64);
    assert!(TLB_FLAG_WRITE <= u32::MAX as u64);
    assert!(TLB_FLAG_EXEC <= u32::MAX as u64);
    assert!(TLB_FLAG_IS_RAM <= u32::MAX as u64);
};

// ---- Default public entry points --------------------------------------------------------------

pub use tier1::{
    discover_block, discover_block_mode, translate_block, BasicBlock, BlockEndKind, BlockLimits,
    Tier1WasmCodegen, Tier1WasmOptions,
};
pub use tier2::{optimize_trace, Tier2WasmCodegen, Tier2WasmOptions, TraceBuilder};

// ---- Legacy baseline compatibility ------------------------------------------------------------

#[cfg(feature = "legacy-baseline")]
pub use legacy::{CpuState, Reg};
