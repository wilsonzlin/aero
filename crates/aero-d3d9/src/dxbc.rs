//! Minimal DXBC container parser.
//!
//! DXBC is the shader container format produced by `fxc`/`d3dcompiler` and used
//! across multiple Direct3D generations. For D3D9 shader model 2/3 the shader
//! token stream is typically stored in the `SHDR` chunk.

use std::collections::HashMap;

use aero_dxbc::DxbcFile;
use thiserror::Error;

#[cfg(feature = "dxbc-robust")]
pub mod robust;

use crate::shader_limits::MAX_D3D9_DXBC_CHUNK_COUNT;

#[derive(Debug, Error)]
pub enum DxbcError {
    #[error("{0}")]
    Shared(#[from] aero_dxbc::DxbcError),
    #[error("buffer too small")]
    BufferTooSmall,
    #[error("missing DXBC magic")]
    BadMagic,
    #[error("DXBC container missing shader bytecode chunk (expected SHDR or SHEX)")]
    MissingShaderChunk,
    #[error("chunk offset out of bounds")]
    ChunkOutOfBounds,
    #[error("chunk size out of bounds")]
    ChunkSizeOutOfBounds,
    #[error("DXBC chunk count {count} exceeds maximum {max}")]
    ChunkCountTooLarge { count: u32, max: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCC(pub u32);

impl FourCC {
    pub const DXBC: FourCC = FourCC(u32::from_le_bytes(*b"DXBC"));
    pub const SHDR: FourCC = FourCC(u32::from_le_bytes(*b"SHDR"));
    pub const SHEX: FourCC = FourCC(u32::from_le_bytes(*b"SHEX"));
    pub const ISGN: FourCC = FourCC(u32::from_le_bytes(*b"ISGN"));
    pub const OSGN: FourCC = FourCC(u32::from_le_bytes(*b"OSGN"));
    pub const RDEF: FourCC = FourCC(u32::from_le_bytes(*b"RDEF"));
    pub const CTAB: FourCC = FourCC(u32::from_le_bytes(*b"CTAB"));

    pub fn as_str(self) -> String {
        let bytes = self.0.to_le_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

fn read_u32_le(bytes: &[u8], offset: &mut usize) -> Result<u32, DxbcError> {
    if *offset + 4 > bytes.len() {
        return Err(DxbcError::BufferTooSmall);
    }
    let val = u32::from_le_bytes(bytes[*offset..*offset + 4].try_into().unwrap());
    *offset += 4;
    Ok(val)
}

#[derive(Debug, Clone)]
pub struct Chunk<'a> {
    pub fourcc: FourCC,
    pub data: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct Container<'a> {
    pub checksum: [u8; 16],
    pub total_size: u32,
    pub chunks: HashMap<FourCC, Chunk<'a>>,
}

impl<'a> Container<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, DxbcError> {
        if bytes.len() < 32 {
            return Err(DxbcError::BufferTooSmall);
        }
        let mut offset = 0usize;
        let magic = read_u32_le(bytes, &mut offset)?;
        if magic != FourCC::DXBC.0 {
            return Err(DxbcError::BadMagic);
        }
        let mut checksum = [0u8; 16];
        checksum.copy_from_slice(&bytes[offset..offset + 16]);
        offset += 16;

        let _one = read_u32_le(bytes, &mut offset)?;
        let total_size = read_u32_le(bytes, &mut offset)?;
        let chunk_count_raw = read_u32_le(bytes, &mut offset)?;
        if chunk_count_raw > MAX_D3D9_DXBC_CHUNK_COUNT {
            return Err(DxbcError::ChunkCountTooLarge {
                count: chunk_count_raw,
                max: MAX_D3D9_DXBC_CHUNK_COUNT,
            });
        }
        let chunk_count = chunk_count_raw as usize;

        // Ensure the chunk-offset table is present before we allocate based on its declared size.
        let offset_table_bytes = chunk_count
            .checked_mul(4)
            .ok_or(DxbcError::ChunkCountTooLarge {
                count: chunk_count_raw,
                max: MAX_D3D9_DXBC_CHUNK_COUNT,
            })?;
        if offset + offset_table_bytes > bytes.len() {
            return Err(DxbcError::BufferTooSmall);
        }

        let mut chunk_offsets = Vec::with_capacity(chunk_count);
        for _ in 0..chunk_count {
            chunk_offsets.push(read_u32_le(bytes, &mut offset)? as usize);
        }

        let mut chunks = HashMap::new();
        for chunk_offset in chunk_offsets {
            if chunk_offset + 8 > bytes.len() {
                return Err(DxbcError::ChunkOutOfBounds);
            }
            let mut local = chunk_offset;
            let fourcc = FourCC(read_u32_le(bytes, &mut local)?);
            let size = read_u32_le(bytes, &mut local)? as usize;
            if local + size > bytes.len() {
                return Err(DxbcError::ChunkSizeOutOfBounds);
            }
            let data = &bytes[local..local + size];
            chunks.insert(fourcc, Chunk { fourcc, data });
        }

        Ok(Self {
            checksum,
            total_size,
            chunks,
        })
    }

    pub fn get(&self, fourcc: FourCC) -> Option<&Chunk<'a>> {
        self.chunks.get(&fourcc)
    }
}

/// If `bytes` is a DXBC container, return the contained `SHEX`/`SHDR` bytecode
/// slice.
/// Otherwise return `bytes` as-is (D3D9 runtime commonly provides raw token
/// streams).
pub fn extract_shader_bytecode(bytes: &[u8]) -> Result<&[u8], DxbcError> {
    if bytes.starts_with(b"DXBC") {
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

/// Build a minimal DXBC container from chunks.
///
/// This is primarily useful for tests and golden vectors.
pub fn build_container(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    // Header:
    // DXBC (4) + checksum (16) + 1 (4) + total_size (4) + chunk_count (4)
    // + offsets (4 * count) then chunks.
    let header_size = 4 + 16 + 4 + 4 + 4 + (4 * chunks.len());
    let mut out =
        Vec::with_capacity(header_size + chunks.iter().map(|c| 8 + c.1.len()).sum::<usize>());
    out.extend_from_slice(b"DXBC");
    out.extend_from_slice(&[0u8; 16]); // checksum - unused for now
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // total size placeholder
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());

    let offsets_pos = out.len();
    out.resize(out.len() + 4 * chunks.len(), 0);

    let mut offsets = Vec::with_capacity(chunks.len());
    for (fourcc, data) in chunks {
        let offset = out.len();
        offsets.push(offset as u32);
        out.extend_from_slice(&fourcc.0.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }

    // Fill offsets
    for (i, offset) in offsets.iter().enumerate() {
        let pos = offsets_pos + i * 4;
        out[pos..pos + 4].copy_from_slice(&offset.to_le_bytes());
    }

    // Fill total size
    let total_size = out.len() as u32;
    let total_size_pos = 4 + 16 + 4;
    out[total_size_pos..total_size_pos + 4].copy_from_slice(&total_size.to_le_bytes());
    out
}
