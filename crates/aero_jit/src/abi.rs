//! `aero_cpu_core::state::CpuState` WASM JIT ABI.
//!
//! This module is the **single source of truth** for the byte offsets that
//! dynamically-generated WASM blocks use to read/write the canonical CPU state
//! stored in linear memory.
//!
//! The canonical state layout is defined by [`aero_cpu_core::state::CpuState`]
//! (and documented there as a stable ABI). The constants here intentionally use
//! `u32` because WebAssembly memory offsets are encoded as 32-bit immediates.
//!
//! If the `aero_cpu_core` CPU state layout ever changes, the unit tests in
//! `crates/aero_jit/tests/abi.rs` are expected to fail loudly so that JIT
//! codegen can be updated in lockstep.

/// Byte offsets of GPRs in [`aero_cpu_core::state::CpuState`], in architectural order.
pub const CPU_GPR_OFF: [u32; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
];

/// Byte offset of `CpuState.rip`.
pub const CPU_RIP_OFF: u32 = 128;

/// Byte offset of `CpuState.rflags`.
pub const CPU_RFLAGS_OFF: u32 = 136;

/// Byte offsets of XMM registers in `CpuState.sse.xmm[i]`.
pub const CPU_XMM_OFF: [u32; 16] = [
    784, 800, 816, 832, 848, 864, 880, 896, 912, 928, 944, 960, 976, 992, 1008, 1024,
];

/// Total size (in bytes) of [`aero_cpu_core::state::CpuState`].
pub const CPU_STATE_SIZE: u32 = 1056;

/// Alignment (in bytes) of [`aero_cpu_core::state::CpuState`].
pub const CPU_STATE_ALIGN: u32 = 16;

