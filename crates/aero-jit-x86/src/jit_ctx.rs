//! JIT-side context shared with generated WASM blocks/traces.
//!
//! Tier-1 blocks receive a separate `jit_ctx_ptr` parameter pointing at a [`JitContext`] instance
//! stored in linear memory. Tier-2 traces currently only receive a `cpu_ptr` pointing at the
//! architectural CPU state, so Tier-2 metadata is stored at a fixed offset relative to `cpu_ptr`.

use crate::{abi, JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE};

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
        let end = base.checked_add(Self::BYTE_SIZE).unwrap_or_else(|| {
            panic!(
                "JitContext write out of bounds: base={base} size={} mem_len={} (overflow)",
                Self::BYTE_SIZE,
                mem.len()
            )
        });
        assert!(
            end <= mem.len(),
            "JitContext write out of bounds: base={base} size={} mem_len={}",
            Self::BYTE_SIZE,
            mem.len()
        );

        let ram_base_off = base.checked_add(Self::RAM_BASE_OFFSET as usize).unwrap_or_else(|| {
            panic!(
                "JitContext ram_base offset overflow: base={base} off={} mem_len={}",
                Self::RAM_BASE_OFFSET,
                mem.len()
            )
        });
        mem[ram_base_off..ram_base_off + 8].copy_from_slice(&self.ram_base.to_le_bytes());

        let tlb_salt_off = base.checked_add(Self::TLB_SALT_OFFSET as usize).unwrap_or_else(|| {
            panic!(
                "JitContext tlb_salt offset overflow: base={base} off={} mem_len={}",
                Self::TLB_SALT_OFFSET,
                mem.len()
            )
        });
        mem[tlb_salt_off..tlb_salt_off + 8].copy_from_slice(&self.tlb_salt.to_le_bytes());
    }
}

const _: () = {
    use core::mem::{offset_of, size_of};

    assert!(JitContext::TOTAL_BYTE_SIZE <= u32::MAX as usize);
    assert!(offset_of!(JitContext, ram_base) == JitContext::RAM_BASE_OFFSET as usize);
    assert!(offset_of!(JitContext, tlb_salt) == JitContext::TLB_SALT_OFFSET as usize);
    assert!(size_of::<JitContext>() == JitContext::BYTE_SIZE);
    assert!(JitContext::TLB_OFFSET as usize == JitContext::BYTE_SIZE);
    assert!(JitContext::TLB_BYTES == JIT_TLB_ENTRIES * (JIT_TLB_ENTRY_SIZE as usize));
    assert!(JitContext::TOTAL_BYTE_SIZE == JitContext::BYTE_SIZE + JitContext::TLB_BYTES);
};

#[cfg(test)]
mod tests {
    use memoffset::offset_of;

    use super::JitContext;

    #[test]
    fn jit_context_layout_matches_constants() {
        assert_eq!(
            offset_of!(JitContext, ram_base) as u32,
            JitContext::RAM_BASE_OFFSET
        );
        assert_eq!(
            offset_of!(JitContext, tlb_salt) as u32,
            JitContext::TLB_SALT_OFFSET
        );

        assert_eq!(core::mem::size_of::<JitContext>(), JitContext::BYTE_SIZE);
        assert_eq!(JitContext::TLB_OFFSET as usize, JitContext::BYTE_SIZE);

        assert_eq!(
            JitContext::TLB_BYTES,
            crate::JIT_TLB_ENTRIES * (crate::JIT_TLB_ENTRY_SIZE as usize)
        );
        assert_eq!(
            JitContext::TOTAL_BYTE_SIZE,
            JitContext::BYTE_SIZE + JitContext::TLB_BYTES
        );
    }

    #[test]
    fn jit_context_write_header_writes_little_endian_fields() {
        let ctx = JitContext {
            ram_base: 0x1122_3344_5566_7788,
            tlb_salt: 0x99aa_bbcc_ddee_ff00,
        };

        let mut mem = [0u8; 64];
        let base = 7usize;
        ctx.write_header_to_mem(&mut mem, base);

        let ram_base_off = base + (JitContext::RAM_BASE_OFFSET as usize);
        assert_eq!(
            &mem[ram_base_off..ram_base_off + 8],
            &ctx.ram_base.to_le_bytes()
        );

        let salt_off = base + (JitContext::TLB_SALT_OFFSET as usize);
        assert_eq!(
            &mem[salt_off..salt_off + 8],
            &ctx.tlb_salt.to_le_bytes()
        );
    }
}

/// Offset (relative to `cpu_ptr`) of the Tier-2 context region.
pub const TIER2_CTX_OFFSET: u32 = abi::CPU_STATE_SIZE + (JitContext::TOTAL_BYTE_SIZE as u32);

/// Offset of the Tier-2 trace exit reason (`u32`).
pub const TRACE_EXIT_REASON_OFFSET: u32 = TIER2_CTX_OFFSET;

/// The trace exited normally (no special handling required).
pub const TRACE_EXIT_REASON_NONE: u32 = 0;

/// The trace exited because a code page version guard failed.
///
/// Runtimes are expected to invalidate the cached Tier-2 trace and resume execution in the
/// interpreter/Tier-1 at the returned RIP.
pub const TRACE_EXIT_REASON_CODE_INVALIDATION: u32 = 1;

/// Offset of a pointer (`u32`, byte offset) to the page-version table.
pub const CODE_VERSION_TABLE_PTR_OFFSET: u32 = TIER2_CTX_OFFSET + 4;

/// Offset of the length (`u32`, number of `u32` entries) of the page-version table.
pub const CODE_VERSION_TABLE_LEN_OFFSET: u32 = TIER2_CTX_OFFSET + 8;

/// Total size (in bytes) of the Tier-2 context region.
pub const TIER2_CTX_SIZE: u32 = 12;

/// Backwards-compatible alias for [`TIER2_CTX_SIZE`].
pub const JIT_CTX_SIZE: u32 = TIER2_CTX_SIZE;
