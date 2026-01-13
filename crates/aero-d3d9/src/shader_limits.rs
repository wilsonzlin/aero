//! Centralized limits for D3D9 shader decoding/parsing.
//!
//! Aero treats guest-provided shader bytecode as untrusted input. These limits bound memory usage
//! and prevent pathological shader blobs from triggering large allocations during decoding.

/// Maximum accepted D3D9 shader bytecode length in bytes.
///
/// The legacy token-stream translator and the SM3 decoder both allocate temporary `Vec<u32>`
/// buffers sized to the incoming bytecode. Keep this small enough to avoid OOM while still being
/// comfortably above real-world SM2/SM3 shader sizes.
pub(crate) const MAX_D3D9_SHADER_BYTECODE_BYTES: usize = 256 * 1024; // 256 KiB

/// Maximum accepted D3D9 shader token count (DWORDs / `u32`s).
pub(crate) const MAX_D3D9_SHADER_TOKEN_COUNT: usize = MAX_D3D9_SHADER_BYTECODE_BYTES / 4;

/// Maximum tolerated register index for any register file (r#/c#/s#/v#/t#/etc).
///
/// Even though the DX9 token encoding can represent register indices up to 2047, the Aero
/// backends are only designed to handle a much smaller subset (e.g. 256 constant registers per
/// stage). Capping indices early prevents hostile inputs from generating huge output shaders or
/// invalid constant-buffer indexing.
pub(crate) const MAX_D3D9_SHADER_REGISTER_INDEX: u32 = 255;

/// Maximum number of chunks tolerated in a DXBC container.
///
/// DXBC chunk counts are stored in the container header and must be treated as untrusted. The
/// production D3D9 path only needs the `SHDR`/`SHEX` chunk, and real-world containers typically
/// contain a small handful of chunks (single digits). This hard cap prevents `Vec::with_capacity`
/// OOM when parsing corrupted containers.
pub(crate) const MAX_D3D9_DXBC_CHUNK_COUNT: u32 = 4096;
