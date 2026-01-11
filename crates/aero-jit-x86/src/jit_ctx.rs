//! JIT-side context shared with generated Tier-1 WASM blocks.
//!
//! This is stored in WASM linear memory separately from the architectural CPU state
//! (`aero_cpu_core::state::CpuState` in the full emulator). Keeping these fields out of the CPU
//! state avoids "polluting" the core ABI while still allowing the Tier-1 JIT to implement a fast
//! inline translation path (direct-mapped TLB + direct RAM loads/stores).

use crate::{JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE};

/// Header for the Tier-1 JIT context.
///
/// The context is stored in linear memory at `jit_ctx_ptr` (WASM i32 byte offset). All integer
/// fields are little-endian.
///
/// Layout (bytes):
/// - `ram_base` (`u64`): base offset of guest RAM within linear memory.
/// - `tlb_salt` (`u64`): tag salt used by the direct-mapped JIT TLB.
/// - `tlb[]` (`JIT_TLB_ENTRIES` × 16 bytes): `{ tag: u64, data: u64 }`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct JitContext {
    pub ram_base: u64,
    pub tlb_salt: u64,
}

impl JitContext {
    pub const RAM_BASE_OFFSET: u32 = 0;
    pub const TLB_SALT_OFFSET: u32 = Self::RAM_BASE_OFFSET + 8;
    pub const TLB_OFFSET: u32 = Self::TLB_SALT_OFFSET + 8;

    /// Size of the fixed header (`ram_base` + `tlb_salt`) in bytes.
    pub const BYTE_SIZE: usize = Self::TLB_OFFSET as usize;

    pub const TLB_BYTES: usize = JIT_TLB_ENTRIES * (JIT_TLB_ENTRY_SIZE as usize);
    pub const TOTAL_BYTE_SIZE: usize = Self::BYTE_SIZE + Self::TLB_BYTES;

    /// Writes just the header fields (`ram_base`, `tlb_salt`) into `mem[base..]`.
    ///
    /// The TLB array is left untouched (typically zeroed on allocation).
    pub fn write_header_to_mem(&self, mem: &mut [u8], base: usize) {
        assert!(
            base + Self::BYTE_SIZE <= mem.len(),
            "JitContext write out of bounds: base={base} size={} mem_len={}",
            Self::BYTE_SIZE,
            mem.len()
        );

        let ram_base_off = base + (Self::RAM_BASE_OFFSET as usize);
        mem[ram_base_off..ram_base_off + 8].copy_from_slice(&self.ram_base.to_le_bytes());

        let tlb_salt_off = base + (Self::TLB_SALT_OFFSET as usize);
        mem[tlb_salt_off..tlb_salt_off + 8].copy_from_slice(&self.tlb_salt.to_le_bytes());
    }
}
