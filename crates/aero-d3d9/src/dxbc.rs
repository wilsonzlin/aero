//! DXBC helpers (DXBC container → D3D9 token stream).
//!
//! DXBC is the shader container format produced by `fxc`/`d3dcompiler` and used
//! across multiple Direct3D generations. For D3D9 shader model 2/3 the shader
//! token stream is typically stored in the `SHDR` chunk.

use aero_dxbc::DxbcFile;
use thiserror::Error;

#[cfg(feature = "dxbc-robust")]
pub mod robust;

use crate::shader_limits::MAX_D3D9_DXBC_CHUNK_COUNT;

#[derive(Debug, Error)]
pub enum DxbcError {
    #[error("{0}")]
    Shared(#[from] aero_dxbc::DxbcError),
    #[error("DXBC container missing shader bytecode chunk (expected SHDR or SHEX)")]
    MissingShaderChunk,
    #[error("DXBC chunk count {count} exceeds maximum {max}")]
    ChunkCountTooLarge { count: u32, max: u32 },
}

/// If `bytes` is a DXBC container, return the contained `SHEX`/`SHDR` bytecode
/// slice.
/// Otherwise return `bytes` as-is (D3D9 runtime commonly provides raw token
/// streams).
pub fn extract_shader_bytecode(bytes: &[u8]) -> Result<&[u8], DxbcError> {
    if bytes.starts_with(b"DXBC") {
        // `aero-dxbc` validates offsets by iterating the declared chunk table. Put a hard cap on
        // `chunk_count` (a guest-controlled value) so corrupted blobs can't force us into a huge
        // amount of work before we even locate the shader bytecode chunk.
        if bytes.len() >= 32 {
            let chunk_count_raw = u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
            if chunk_count_raw > MAX_D3D9_DXBC_CHUNK_COUNT {
                return Err(DxbcError::ChunkCountTooLarge {
                    count: chunk_count_raw,
                    max: MAX_D3D9_DXBC_CHUNK_COUNT,
                });
            }
        }

        // Use the shared `aero-dxbc` parser for runtime DXBC validation and
        // chunk extraction. This is strict about bounds and respects the
        // declared `total_size` when slicing.
        let dxbc = DxbcFile::parse(bytes)?;
        let Some(shader_chunk) = dxbc.find_first_shader_chunk() else {
            return Err(DxbcError::MissingShaderChunk);
        };
        Ok(shader_chunk.data)
    } else {
        Ok(bytes)
    }
}
