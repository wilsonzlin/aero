/// Module name for all imports required by Aero JIT-generated WASM modules.
pub const IMPORT_MODULE: &str = "env";

/// Imported linear memory (`WebAssembly.Memory`) shared with the main emulator.
pub const IMPORT_MEMORY: &str = "memory";

/// Export name used by compiled Tier-1 blocks (and the legacy baseline codegen).
pub const EXPORT_BLOCK_FN: &str = "block";

// Slow-path memory helpers.
pub const IMPORT_MEM_READ_U8: &str = "mem_read_u8";
pub const IMPORT_MEM_READ_U16: &str = "mem_read_u16";
pub const IMPORT_MEM_READ_U32: &str = "mem_read_u32";
pub const IMPORT_MEM_READ_U64: &str = "mem_read_u64";
pub const IMPORT_MEM_WRITE_U8: &str = "mem_write_u8";
pub const IMPORT_MEM_WRITE_U16: &str = "mem_write_u16";
pub const IMPORT_MEM_WRITE_U32: &str = "mem_write_u32";
pub const IMPORT_MEM_WRITE_U64: &str = "mem_write_u64";

/// Page-fault helper for the baseline ABI.
pub const IMPORT_PAGE_FAULT: &str = "page_fault";

/// Slow-path address translation helper.
///
/// Called on JIT TLB miss or permission failure. The runtime is expected to:
/// - translate the virtual address
/// - fill the corresponding JIT TLB entry in linear memory
/// - return the packed `{phys_page_base | flags}` word used by the fast-path
pub const IMPORT_MMU_TRANSLATE: &str = "mmu_translate";

/// Exit helper used when a translated access resolves to MMIO/ROM/unmapped instead of RAM.
pub const IMPORT_JIT_EXIT_MMIO: &str = "jit_exit_mmio";

/// Bailout helper used to exit back to the runtime on unsupported IR ops or explicit bailout.
pub const IMPORT_JIT_EXIT: &str = "jit_exit";

/// Sentinel return value (`u64::MAX` as `i64`) used by Tier-1 codegen to request an
/// interpreter fallback while still writing the precise `next_rip` into `CpuState.rip`.
pub const JIT_EXIT_SENTINEL_I64: i64 = -1i64;
