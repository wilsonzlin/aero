//! `aero_cpu_core::state::CpuState` WASM JIT ABI.
//!
//! Exposes byte offsets into the canonical CPU state struct stored in linear memory. Offsets are
//! `u32` because WASM encodes memory immediates as 32-bit.
//!
//! Tier-1 blocks may additionally use a JIT-only context region appended immediately after the
//! CPU state (see `JIT_CTX_*` below) to implement an inline direct-mapped JIT TLB fast-path.

use crate::{JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE};

use wasm_encoder::MemArg;

const fn cast_usize_array_16(src: [usize; 16]) -> [u32; 16] {
    let mut out = [0u32; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = src[i] as u32;
        i += 1;
    }
    out
}

const _: () = {
    let mut i = 0;
    while i < 16 {
        assert!(aero_cpu_core::state::CPU_GPR_OFF[i] <= u32::MAX as usize);
        assert!(aero_cpu_core::state::CPU_XMM_OFF[i] <= u32::MAX as usize);
        i += 1;
    }

    assert!(aero_cpu_core::state::CPU_RIP_OFF <= u32::MAX as usize);
    assert!(aero_cpu_core::state::CPU_RFLAGS_OFF <= u32::MAX as usize);
    assert!(aero_cpu_core::state::CPU_STATE_SIZE <= u32::MAX as usize);
    assert!(aero_cpu_core::state::CPU_STATE_ALIGN <= u32::MAX as usize);
};

pub const CPU_GPR_OFF: [u32; 16] = cast_usize_array_16(aero_cpu_core::state::CPU_GPR_OFF);
pub const CPU_RIP_OFF: u32 = aero_cpu_core::state::CPU_RIP_OFF as u32;
pub const CPU_RFLAGS_OFF: u32 = aero_cpu_core::state::CPU_RFLAGS_OFF as u32;
pub const CPU_XMM_OFF: [u32; 16] = cast_usize_array_16(aero_cpu_core::state::CPU_XMM_OFF);
pub const CPU_STATE_SIZE: u32 = aero_cpu_core::state::CPU_STATE_SIZE as u32;
pub const CPU_STATE_ALIGN: u32 = aero_cpu_core::state::CPU_STATE_ALIGN as u32;

#[inline]
pub fn gpr_offset(gpr_index: usize) -> u32 {
    CPU_GPR_OFF[gpr_index]
}

#[inline]
pub fn memarg(offset: u32, align: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align,
        memory_index: 0,
    }
}

/// Offset from `cpu_ptr` to the start of the JIT context region.
pub const JIT_CTX_OFFSET: u32 = CPU_STATE_SIZE;

pub const JIT_CTX_RAM_BASE_OFFSET: u32 = JIT_CTX_OFFSET + 0;
pub const JIT_CTX_TLB_SALT_OFFSET: u32 = JIT_CTX_OFFSET + 8;
pub const JIT_CTX_TLB_OFFSET: u32 = JIT_CTX_OFFSET + 16;

pub const JIT_CTX_PREFIX_SIZE: u32 = 16;
pub const JIT_CTX_TLB_BYTES: u32 = (JIT_TLB_ENTRIES as u32) * JIT_TLB_ENTRY_SIZE;
pub const JIT_CTX_BYTE_SIZE: u32 = JIT_CTX_PREFIX_SIZE + JIT_CTX_TLB_BYTES;

/// Total size of `CpuState + JitContext` (bytes).
pub const CPU_AND_JIT_CTX_BYTE_SIZE: u32 = CPU_STATE_SIZE + JIT_CTX_BYTE_SIZE;

/// `access` codes passed to the `mmu_translate(cpu_ptr, vaddr, access)` import.
pub const MMU_ACCESS_READ: i32 = 0;
pub const MMU_ACCESS_WRITE: i32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct JitTlbEntry {
    pub tag: u64,
    pub data: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct JitContext {
    pub ram_base: u64,
    pub tlb_salt: u64,
    pub tlb_entries: [JitTlbEntry; JIT_TLB_ENTRIES],
}

impl Default for JitContext {
    fn default() -> Self {
        Self {
            ram_base: 0,
            tlb_salt: 0,
            tlb_entries: [JitTlbEntry::default(); JIT_TLB_ENTRIES],
        }
    }
}
