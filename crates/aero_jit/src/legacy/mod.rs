//! Legacy baseline JIT pipeline (inline TLB + legacy IR).
//!
//! This code is kept behind the `legacy-baseline` feature to avoid polluting the
//! default `aero_jit` API surface.

pub mod cpu;
pub mod interp;
pub mod ir;

pub use cpu::{
    CpuState, Reg, JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, JIT_TLB_INDEX_MASK, PAGE_BASE_MASK,
    PAGE_OFFSET_MASK, PAGE_SHIFT, PAGE_SIZE, TLB_FLAG_EXEC, TLB_FLAG_IS_RAM, TLB_FLAG_READ,
    TLB_FLAG_WRITE,
};

pub use crate::wasm::legacy as wasm;
