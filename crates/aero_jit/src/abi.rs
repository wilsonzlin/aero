//! `aero_cpu_core::state::CpuState` WASM JIT ABI.
//!
//! Exposes byte offsets into the canonical CPU state struct stored in linear memory. Offsets are
//! `u32` because WASM encodes memory immediates as 32-bit.

use wasm_encoder::MemArg;

pub const GPR_COUNT: usize = aero_cpu_core::state::GPR_COUNT;

/// Reserved RFLAGS bit 1. Always reads as 1 on real hardware.
pub const RFLAGS_RESERVED1: u64 = aero_cpu_core::state::RFLAGS_RESERVED1;

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

/// `access` codes passed to the `mmu_translate(cpu_ptr, jit_ctx_ptr, vaddr, access)` import.
pub const MMU_ACCESS_READ: i32 = 0;
pub const MMU_ACCESS_WRITE: i32 = 1;
