//! Parsers for DXBC signature chunks (`ISGN`, `OSGN`, `PSGN`, ...).
//!
//! Signature chunks provide semantic/register mappings for shader inputs and
//! outputs in shader model 4 and newer.

use crate::DxbcError;

const SIGNATURE_HEADER_LEN: usize = 8;
const SIGNATURE_ENTRY_LEN_V0: usize = 24;
const SIGNATURE_ENTRY_LEN_V1: usize = 32;

/// A parsed DXBC signature chunk (`ISGN`, `OSGN`, `PSGN`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureChunk {
    /// Parsed signature entries.
    pub entries: Vec<SignatureEntry>,
}

/// A single entry in a DXBC signature chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureEntry {
    /// The semantic name (e.g. `"POSITION"` or `"TEXCOORD"`).
    pub semantic_name: String,
    /// The semantic index (e.g. `0` for `TEXCOORD0`).
    pub semantic_index: u32,
    /// Register index assigned by the compiler.
    pub register: u32,
    /// System value type (`D3D_NAME`) stored as a raw `u32`.
    pub system_value_type: u32,
    /// Register component type (`D3D_REGISTER_COMPONENT_TYPE`) stored as a raw `u32`.
    pub component_type: u32,
    /// Component presence mask (`D3D_COMPONENT_MASK`) stored as a raw `u8`.
    pub mask: u8,
    /// Read/write mask stored as a raw `u8`.
    pub read_write_mask: u8,
    /// Stream index (used by geometry shaders), if present in the encoding.
    pub stream: Option<u32>,
}

/// Parses a DXBC signature chunk payload.
///
/// This function expects the chunk payload bytes (the data following the chunk's
/// `FourCC` and size fields inside the DXBC container).
pub fn parse_signature_chunk(bytes: &[u8]) -> Result<SignatureChunk, DxbcError> {
    if bytes.len() < SIGNATURE_HEADER_LEN {
        return Err(DxbcError::invalid_chunk(format!(
            "signature chunk is truncated: need {SIGNATURE_HEADER_LEN} bytes for header, got {}",
            bytes.len()
        )));
    }

    let param_count = read_u32_le(bytes, 0, "param_count")?;
    let param_offset = read_u32_le(bytes, 4, "param_offset")?;

    let param_count_usize = param_count as usize;
    let param_offset_usize = param_offset as usize;

    if param_count_usize == 0 {
        if param_offset_usize > bytes.len() {
            return Err(DxbcError::invalid_chunk(format!(
                "param_offset {param_offset} is outside chunk length {}",
                bytes.len()
            )));
        }
        return Ok(SignatureChunk { entries: Vec::new() });
    }

    if param_offset_usize < SIGNATURE_HEADER_LEN {
        return Err(DxbcError::invalid_chunk(format!(
            "param_offset {param_offset} points into signature header (need >= {SIGNATURE_HEADER_LEN})"
        )));
    }
    if (param_offset_usize % 4) != 0 {
        return Err(DxbcError::invalid_chunk(format!(
            "param_offset {param_offset} is not 4-byte aligned"
        )));
    }

    // Most DXBC signatures use 24-byte entries, but some toolchains emit a 32-byte
    // variant (carrying stream/min-precision as DWORDs). Try the common format
    // first, then fall back to the larger entry size if needed.
    parse_signature_chunk_with_entry_size(bytes, param_count, param_offset, SIGNATURE_ENTRY_LEN_V0)
        .or_else(|err_v0| {
            parse_signature_chunk_with_entry_size(
                bytes,
                param_count,
                param_offset,
                SIGNATURE_ENTRY_LEN_V1,
            )
            .or(Err(err_v0))
        })
}

fn parse_signature_chunk_with_entry_size(
    bytes: &[u8],
    param_count: u32,
    param_offset: u32,
    entry_size: usize,
) -> Result<SignatureChunk, DxbcError> {
    let param_count_usize = param_count as usize;
    let param_offset_usize = param_offset as usize;

    let table_bytes = param_count_usize
        .checked_mul(entry_size)
        .ok_or_else(|| DxbcError::invalid_chunk("signature parameter count overflows table size"))?;

    let table_end = param_offset_usize
        .checked_add(table_bytes)
        .ok_or_else(|| DxbcError::invalid_chunk("signature table end overflows"))?;

    if table_end > bytes.len() {
        return Err(DxbcError::invalid_chunk(format!(
            "signature table at {param_offset_usize}..{table_end} is outside chunk length {}",
            bytes.len()
        )));
    }

    let mut entries = Vec::new();
    entries
        .try_reserve_exact(param_count_usize)
        .map_err(|_| {
            DxbcError::invalid_chunk(format!(
                "signature entry count {param_count} is too large to allocate"
            ))
        })?;

    for entry_index in 0..param_count_usize {
        let entry_offset = entry_index.checked_mul(entry_size).ok_or_else(|| {
            DxbcError::invalid_chunk(format!("signature entry {entry_index} offset overflows"))
        })?;
        let entry_start = param_offset_usize.checked_add(entry_offset).ok_or_else(|| {
            DxbcError::invalid_chunk(format!("signature entry {entry_index} start overflows"))
        })?;

        let semantic_name_offset = read_u32_le(
            bytes,
            entry_start,
            &format!("entry {entry_index} semantic_name_offset"),
        )?;
        let semantic_name_offset_usize = semantic_name_offset as usize;
        if semantic_name_offset_usize < SIGNATURE_HEADER_LEN {
            return Err(DxbcError::invalid_chunk(format!(
                "entry {entry_index} semantic_name_offset {semantic_name_offset} points into signature header"
            )));
        }
        if (param_offset_usize..table_end).contains(&semantic_name_offset_usize) {
            return Err(DxbcError::invalid_chunk(format!(
                "entry {entry_index} semantic_name_offset {semantic_name_offset} points into signature table ({param_offset_usize}..{table_end})"
            )));
        }

        let semantic_index = read_u32_le(
            bytes,
            entry_start + 4,
            &format!("entry {entry_index} semantic_index"),
        )?;
        let system_value_type = read_u32_le(
            bytes,
            entry_start + 8,
            &format!("entry {entry_index} system_value_type"),
        )?;
        let component_type = read_u32_le(
            bytes,
            entry_start + 12,
            &format!("entry {entry_index} component_type"),
        )?;
        let register = read_u32_le(
            bytes,
            entry_start + 16,
            &format!("entry {entry_index} register"),
        )?;

        let (mask, read_write_mask, stream) = match entry_size {
            SIGNATURE_ENTRY_LEN_V0 => {
                // The last DWORD is packed as 4 bytes:
                // - mask
                // - read_write_mask
                // - stream
                // - min_precision (ignored)
                let packed = read_u32_le(
                    bytes,
                    entry_start + 20,
                    &format!("entry {entry_index} mask/rw_mask/stream"),
                )?;
                (
                    (packed & 0xFF) as u8,
                    ((packed >> 8) & 0xFF) as u8,
                    ((packed >> 16) & 0xFF) as u32,
                )
            }
            SIGNATURE_ENTRY_LEN_V1 => {
                // 32-byte variant: mask/rw bytes followed by stream/min-precision DWORDs.
                let mask = *bytes.get(entry_start + 20).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "need 1 byte for entry {entry_index} mask at {}",
                        entry_start + 20
                    ))
                })?;
                let read_write_mask = *bytes.get(entry_start + 21).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "need 1 byte for entry {entry_index} read_write_mask at {}",
                        entry_start + 21
                    ))
                })?;
                let stream =
                    read_u32_le(bytes, entry_start + 24, &format!("entry {entry_index} stream"))?;
                (mask, read_write_mask, stream)
            }
            other => {
                return Err(DxbcError::invalid_chunk(format!(
                    "unsupported signature entry size {other}"
                )))
            }
        };

        let semantic_name = read_cstring(
            bytes,
            semantic_name_offset_usize,
            &format!("entry {entry_index} semantic_name"),
        )?
        .to_owned();

        entries.push(SignatureEntry {
            semantic_name,
            semantic_index,
            register,
            system_value_type,
            component_type,
            mask,
            read_write_mask,
            stream: Some(stream),
        });
    }

    Ok(SignatureChunk { entries })
}

fn read_u32_le(bytes: &[u8], offset: usize, what: &str) -> Result<u32, DxbcError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| DxbcError::invalid_chunk(format!("{what} offset overflows")))?;
    let slice = bytes.get(offset..end).ok_or_else(|| {
        DxbcError::invalid_chunk(format!(
            "need 4 bytes for {what} at {offset}..{end}, but chunk length is {}",
            bytes.len()
        ))
    })?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_cstring<'a>(bytes: &'a [u8], offset: usize, what: &str) -> Result<&'a str, DxbcError> {
    let tail = bytes.get(offset..).ok_or_else(|| {
        DxbcError::invalid_chunk(format!(
            "{what} offset {offset} is outside chunk length {}",
            bytes.len()
        ))
    })?;
    let nul = tail.iter().position(|&b| b == 0).ok_or_else(|| {
        DxbcError::invalid_chunk(format!(
            "{what} at offset {offset} is missing a null terminator"
        ))
    })?;

    let str_bytes = &tail[..nul];
    core::str::from_utf8(str_bytes).map_err(|_| {
        DxbcError::invalid_chunk(format!("{what} at offset {offset} is not valid UTF-8"))
    })
}
