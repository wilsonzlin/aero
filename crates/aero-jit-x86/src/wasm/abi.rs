/// Module name for all imports required by Aero JIT-generated WASM modules.
pub const IMPORT_MODULE: &str = "env";

/// Imported linear memory (`WebAssembly.Memory`) shared with the main emulator.
pub const IMPORT_MEMORY: &str = "memory";

/// Maximum number of 64KiB pages in a wasm32 linear memory (4GiB).
///
/// Shared memories require an explicit maximum. Defaulting to the wasm32 limit lets generated
/// modules link against any smaller shared memory provided by the host.
pub const WASM32_MAX_PAGES: u32 = 65_536;

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

/// Import that returns the current code page version for self-modifying code guards.
///
/// Signature: `env.code_page_version(cpu_ptr: i32, page: i64) -> i64`, returning a
/// `u32`-encoded-as-`i64`.
pub const IMPORT_CODE_PAGE_VERSION: &str = "code_page_version";

/// Page-fault helper for the baseline ABI.
pub const IMPORT_PAGE_FAULT: &str = "page_fault";

/// Slow-path address translation helper.
///
/// Called on JIT TLB miss or permission failure. The runtime is expected to:
/// - translate the virtual address
/// - fill the corresponding JIT TLB entry in linear memory
/// - return the packed `{phys_page_base | flags}` word used by the fast-path
///
/// Signature differs by codegen tier:
/// - Tier-1: `mmu_translate(cpu_ptr, jit_ctx_ptr, vaddr, access) -> i64`
/// - Legacy baseline: `mmu_translate(cpu_ptr, vaddr, access) -> i64`
pub const IMPORT_MMU_TRANSLATE: &str = "mmu_translate";

/// Exit helper used when a translated access resolves to MMIO/ROM/unmapped instead of RAM.
pub const IMPORT_JIT_EXIT_MMIO: &str = "jit_exit_mmio";

/// Bailout helper used to exit back to the runtime on unsupported IR ops or explicit bailout.
pub const IMPORT_JIT_EXIT: &str = "jit_exit";

/// Sentinel return value (`u64::MAX` as `i64`) used by Tier-1 codegen to request an
/// interpreter fallback while still writing the precise `next_rip` into `CpuState.rip`.
pub const JIT_EXIT_SENTINEL_I64: i64 = -1i64;
