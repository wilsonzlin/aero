//! Minimal DXBC container parser.
//!
//! DXBC is the shader container format produced by `fxc`/`d3dcompiler` and used
//! across multiple Direct3D generations. For D3D9 shader model 2/3 the shader
//! token stream is typically stored in the `SHDR` chunk.

use std::collections::HashMap;

use thiserror::Error;

pub mod robust;

#[derive(Debug, Error)]
pub enum DxbcError {
    #[error("buffer too small")]
    BufferTooSmall,
    #[error("missing DXBC magic")]
    BadMagic,
    #[error("chunk offset out of bounds")]
    ChunkOutOfBounds,
    #[error("chunk size out of bounds")]
    ChunkSizeOutOfBounds,
}

#[derive(Debug, Error)]
pub enum ChunkParseError {
    #[error("buffer too small")]
    BufferTooSmall,
    #[error("offset out of bounds")]
    OffsetOutOfBounds,
    #[error("string out of bounds")]
    StringOutOfBounds,
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

fn read_u32_le_chunk(bytes: &[u8], offset: &mut usize) -> Result<u32, ChunkParseError> {
    if *offset + 4 > bytes.len() {
        return Err(ChunkParseError::BufferTooSmall);
    }
    let val = u32::from_le_bytes(bytes[*offset..*offset + 4].try_into().unwrap());
    *offset += 4;
    Ok(val)
}

fn read_u16_le_chunk(bytes: &[u8], offset: &mut usize) -> Result<u16, ChunkParseError> {
    if *offset + 2 > bytes.len() {
        return Err(ChunkParseError::BufferTooSmall);
    }
    let val = u16::from_le_bytes(bytes[*offset..*offset + 2].try_into().unwrap());
    *offset += 2;
    Ok(val)
}

fn read_cstr(bytes: &[u8], offset: usize) -> Result<String, ChunkParseError> {
    if offset >= bytes.len() {
        return Err(ChunkParseError::StringOutOfBounds);
    }
    let tail = &bytes[offset..];
    let nul = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or(ChunkParseError::StringOutOfBounds)?;
    Ok(String::from_utf8_lossy(&tail[..nul]).into_owned())
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
        let chunk_count = read_u32_le(bytes, &mut offset)? as usize;

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

/// If `bytes` is a DXBC container, return the contained `SHDR` bytecode slice.
/// Otherwise return `bytes` as-is (D3D9 runtime commonly provides raw token
/// streams).
pub fn extract_shader_bytecode(bytes: &[u8]) -> Result<&[u8], DxbcError> {
    if bytes.len() >= 4 && &bytes[0..4] == b"DXBC" {
        let container = Container::parse(bytes)?;
        if let Some(shdr) = container
            .get(FourCC::SHDR)
            .or_else(|| container.get(FourCC::SHEX))
        {
            Ok(shdr.data)
        } else {
            Ok(bytes)
        }
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

/// A single element of an input/output signature chunk (`ISGN`/`OSGN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureElement {
    pub semantic: String,
    pub semantic_index: u32,
    pub register: u32,
    pub mask: u8,
}

/// Parse a DXBC signature chunk (`ISGN`/`OSGN`).
pub fn parse_signature(chunk: &[u8]) -> Result<Vec<SignatureElement>, ChunkParseError> {
    let mut off = 0usize;
    let count = read_u32_le_chunk(chunk, &mut off)? as usize;
    let table_offset = read_u32_le_chunk(chunk, &mut off)? as usize;
    let entry_size = 24usize;
    if table_offset + count * entry_size > chunk.len() {
        return Err(ChunkParseError::OffsetOutOfBounds);
    }

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut eoff = table_offset + i * entry_size;
        let name_offset = read_u32_le_chunk(chunk, &mut eoff)? as usize;
        let semantic_index = read_u32_le_chunk(chunk, &mut eoff)?;
        let _system_value_type = read_u32_le_chunk(chunk, &mut eoff)?;
        let _component_type = read_u32_le_chunk(chunk, &mut eoff)?;
        let register = read_u32_le_chunk(chunk, &mut eoff)?;
        if eoff + 4 > chunk.len() {
            return Err(ChunkParseError::BufferTooSmall);
        }
        let mask = chunk[eoff];
        let _rw_mask = chunk[eoff + 1];
        let semantic = read_cstr(chunk, name_offset)?;
        out.push(SignatureElement {
            semantic,
            semantic_index,
            register,
            mask,
        });
    }
    Ok(out)
}

/// A single bound resource entry from the `RDEF` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBinding {
    pub name: String,
    pub bind_point: u32,
    pub bind_count: u32,
    pub ty: u32,
    pub dimension: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDefs {
    pub creator: Option<String>,
    pub resources: Vec<ResourceBinding>,
}

/// Parse a DXBC resource definition chunk (`RDEF`).
///
/// This intentionally parses only what the D3D9→WebGPU layer needs: bound
/// resources and their binding points. Constant buffers are left for future work.
pub fn parse_rdef(chunk: &[u8]) -> Result<ResourceDefs, ChunkParseError> {
    let mut off = 0usize;
    if chunk.len() < 28 {
        return Err(ChunkParseError::BufferTooSmall);
    }
    let _cb_count = read_u32_le_chunk(chunk, &mut off)? as usize;
    let _cb_offset = read_u32_le_chunk(chunk, &mut off)? as usize;
    let res_count = read_u32_le_chunk(chunk, &mut off)? as usize;
    let res_offset = read_u32_le_chunk(chunk, &mut off)? as usize;
    let _shader_model = read_u32_le_chunk(chunk, &mut off)?;
    let _flags = read_u32_le_chunk(chunk, &mut off)?;
    let creator_offset = read_u32_le_chunk(chunk, &mut off)? as usize;

    let creator = if creator_offset != 0 {
        Some(read_cstr(chunk, creator_offset)?)
    } else {
        None
    };

    let entry_size = 32usize;
    if res_offset + res_count * entry_size > chunk.len() {
        return Err(ChunkParseError::OffsetOutOfBounds);
    }
    let mut resources = Vec::with_capacity(res_count);
    for i in 0..res_count {
        let mut eoff = res_offset + i * entry_size;
        let name_offset = read_u32_le_chunk(chunk, &mut eoff)? as usize;
        let ty = read_u32_le_chunk(chunk, &mut eoff)?;
        let _return_type = read_u32_le_chunk(chunk, &mut eoff)?;
        let dimension = read_u32_le_chunk(chunk, &mut eoff)?;
        let _num_samples = read_u32_le_chunk(chunk, &mut eoff)?;
        let bind_point = read_u32_le_chunk(chunk, &mut eoff)?;
        let bind_count = read_u32_le_chunk(chunk, &mut eoff)?;
        let _flags = read_u32_le_chunk(chunk, &mut eoff)?;
        let name = read_cstr(chunk, name_offset)?;
        resources.push(ResourceBinding {
            name,
            bind_point,
            bind_count,
            ty,
            dimension,
        });
    }
    Ok(ResourceDefs { creator, resources })
}

/// A single constant entry from the legacy D3D9 constant table chunk (`CTAB`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtabConstant {
    pub name: String,
    pub register_index: u16,
    pub register_count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantTable {
    pub creator: Option<String>,
    pub target: Option<String>,
    pub constants: Vec<CtabConstant>,
}

/// Parse the legacy D3D9 `CTAB` chunk.
///
/// The full CTAB format is significantly richer; we parse just enough to map
/// constant register ranges back to names when debugging.
pub fn parse_ctab(chunk: &[u8]) -> Result<ConstantTable, ChunkParseError> {
    if chunk.len() < 28 {
        return Err(ChunkParseError::BufferTooSmall);
    }
    let mut off = 0usize;
    let _size = read_u32_le_chunk(chunk, &mut off)? as usize;
    let creator_offset = read_u32_le_chunk(chunk, &mut off)? as usize;
    let _version = read_u32_le_chunk(chunk, &mut off)?;
    let constant_count = read_u32_le_chunk(chunk, &mut off)? as usize;
    let constant_offset = read_u32_le_chunk(chunk, &mut off)? as usize;
    let _flags = read_u32_le_chunk(chunk, &mut off)?;
    let target_offset = read_u32_le_chunk(chunk, &mut off)? as usize;

    let creator = if creator_offset != 0 {
        Some(read_cstr(chunk, creator_offset)?)
    } else {
        None
    };
    let target = if target_offset != 0 {
        Some(read_cstr(chunk, target_offset)?)
    } else {
        None
    };

    let entry_size = 20usize;
    if constant_offset + constant_count * entry_size > chunk.len() {
        return Err(ChunkParseError::OffsetOutOfBounds);
    }
    let mut constants = Vec::with_capacity(constant_count);
    for i in 0..constant_count {
        let mut eoff = constant_offset + i * entry_size;
        let name_offset = read_u32_le_chunk(chunk, &mut eoff)? as usize;
        let _register_set = read_u16_le_chunk(chunk, &mut eoff)?;
        let register_index = read_u16_le_chunk(chunk, &mut eoff)?;
        let register_count = read_u16_le_chunk(chunk, &mut eoff)?;
        let _reserved = read_u16_le_chunk(chunk, &mut eoff)?;
        let _type_info_offset = read_u32_le_chunk(chunk, &mut eoff)? as usize;
        let _default_value_offset = read_u32_le_chunk(chunk, &mut eoff)? as usize;
        let name = read_cstr(chunk, name_offset)?;
        constants.push(CtabConstant {
            name,
            register_index,
            register_count,
        });
    }

    Ok(ConstantTable {
        creator,
        target,
        constants,
    })
}
