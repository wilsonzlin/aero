//! Parsers for DXBC signature chunks (`ISGN`, `OSGN`, `PSGN`, ...).
//!
//! Signature chunks provide semantic/register mappings for shader inputs and
//! outputs in shader model 4 and newer.

use crate::fourcc::FourCC;
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
    ///
    /// Note: all known D3D10+ signature encodings include a stream field; this
    /// is modeled as an `Option` because some callers only care about streams
    /// for geometry/tessellation shaders.
    pub stream: Option<u32>,
}

/// Parses a DXBC signature chunk payload.
///
/// This function expects the chunk payload bytes (the data following the chunk's
/// `FourCC` and size fields inside the DXBC container).
///
/// The container format has two commonly-observed entry layouts:
/// - 24-byte entries (classic `*SGN` chunks).
/// - 32-byte entries (some toolchains emit `*SG1` chunks).
///
/// [`DxbcFile::get_signature`](crate::DxbcFile::get_signature) passes the chunk
/// FourCC to the parser to prefer the matching layout for `*SG1`, but this
/// standalone function also contains a conservative heuristic to auto-detect
/// the 32-byte layout.
pub fn parse_signature_chunk(bytes: &[u8]) -> Result<SignatureChunk, DxbcError> {
    parse_signature_chunk_impl(None, bytes)
}

/// Parses a DXBC signature chunk payload, using the chunk `fourcc` to prefer the
/// correct entry layout (`*SGN` vs `*SG1`) without relying on heuristics.
pub fn parse_signature_chunk_with_fourcc(
    fourcc: FourCC,
    bytes: &[u8],
) -> Result<SignatureChunk, DxbcError> {
    parse_signature_chunk_impl(Some(fourcc), bytes)
}

/// Parses a DXBC signature chunk payload, using the chunk `fourcc` as a hint.
///
/// This is the same as [`parse_signature_chunk_with_fourcc`], but kept as a
/// separate entry-point because some callers want an explicit “for FourCC”
/// API when working with multiple chunk variants (`ISGN`/`ISG1`, etc).
pub fn parse_signature_chunk_for_fourcc(
    fourcc: FourCC,
    bytes: &[u8],
) -> Result<SignatureChunk, DxbcError> {
    parse_signature_chunk_with_fourcc(fourcc, bytes)
}

fn parse_signature_chunk_impl(
    fourcc: Option<FourCC>,
    bytes: &[u8],
) -> Result<SignatureChunk, DxbcError> {
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
        return Ok(SignatureChunk {
            entries: Vec::new(),
        });
    }

    if param_offset_usize < SIGNATURE_HEADER_LEN {
        return Err(DxbcError::invalid_chunk(format!(
            "param_offset {param_offset} points into signature header (need >= {SIGNATURE_HEADER_LEN})"
        )));
    }
    if !param_offset_usize.is_multiple_of(4) {
        return Err(DxbcError::invalid_chunk(format!(
            "param_offset {param_offset} is not 4-byte aligned"
        )));
    }

    // Prefer the encoding indicated by the FourCC when available (`*SGN` vs
    // `*SG1`). Only fall back to a heuristic when the FourCC isn't provided (or
    // doesn't match either known suffix).
    let prefer_v1 = match fourcc {
        Some(f) if f.0[3] == b'1' => true,
        Some(f) if f.0[3] == b'N' => false,
        _ => detect_v1_layout(bytes, param_offset_usize),
    };

    let (first_size, second_size, first_name, second_name) = if prefer_v1 {
        (
            SIGNATURE_ENTRY_LEN_V1,
            SIGNATURE_ENTRY_LEN_V0,
            "v1 32-byte layout",
            "v0 24-byte layout",
        )
    } else {
        (
            SIGNATURE_ENTRY_LEN_V0,
            SIGNATURE_ENTRY_LEN_V1,
            "v0 24-byte layout",
            "v1 32-byte layout",
        )
    };

    match parse_signature_chunk_with_entry_size(bytes, param_count, param_offset, first_size) {
        Ok(chunk) => Ok(chunk),
        Err(err_first) => match parse_signature_chunk_with_entry_size(
            bytes,
            param_count,
            param_offset,
            second_size,
        ) {
            Ok(chunk) => Ok(chunk),
            Err(err_second) => Err(DxbcError::invalid_chunk(format!(
                "failed to parse signature entries ({first_name}: {}; {second_name}: {})",
                err_first.context(),
                err_second.context()
            ))),
        },
    }
}

fn parse_signature_chunk_with_entry_size(
    bytes: &[u8],
    param_count: u32,
    param_offset: u32,
    entry_size: usize,
) -> Result<SignatureChunk, DxbcError> {
    let param_count_usize = param_count as usize;
    let param_offset_usize = param_offset as usize;

    let table_bytes = param_count_usize.checked_mul(entry_size).ok_or_else(|| {
        DxbcError::invalid_chunk("signature parameter count overflows table size")
    })?;

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
    entries.try_reserve_exact(param_count_usize).map_err(|_| {
        DxbcError::invalid_chunk(format!(
            "signature entry count {param_count} is too large to allocate"
        ))
    })?;

    for entry_index in 0..param_count_usize {
        let entry_offset = entry_index.checked_mul(entry_size).ok_or_else(|| {
            DxbcError::invalid_chunk(format!("signature entry {entry_index} offset overflows"))
        })?;
        let entry_start = param_offset_usize
            .checked_add(entry_offset)
            .ok_or_else(|| {
                DxbcError::invalid_chunk(format!("signature entry {entry_index} start overflows"))
            })?;

        let semantic_name_offset =
            read_u32_le_entry(bytes, entry_start, entry_index, "semantic_name_offset")?;
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

        let semantic_index = read_u32_le_entry(
            bytes,
            entry_start.checked_add(4).ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "signature entry {entry_index} semantic_index offset overflows"
                ))
            })?,
            entry_index,
            "semantic_index",
        )?;
        let system_value_type = read_u32_le_entry(
            bytes,
            entry_start.checked_add(8).ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "signature entry {entry_index} system_value_type offset overflows"
                ))
            })?,
            entry_index,
            "system_value_type",
        )?;
        let component_type = read_u32_le_entry(
            bytes,
            entry_start.checked_add(12).ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "signature entry {entry_index} component_type offset overflows"
                ))
            })?,
            entry_index,
            "component_type",
        )?;
        let register = read_u32_le_entry(
            bytes,
            entry_start.checked_add(16).ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "signature entry {entry_index} register offset overflows"
                ))
            })?,
            entry_index,
            "register",
        )?;

        let (mask, read_write_mask, stream) = match entry_size {
            SIGNATURE_ENTRY_LEN_V0 => {
                // The last DWORD is packed as 4 bytes:
                // - mask
                // - read_write_mask
                // - stream
                // - min_precision (ignored)
                let packed = read_u32_le_entry(
                    bytes,
                    entry_start.checked_add(20).ok_or_else(|| {
                        DxbcError::invalid_chunk(format!(
                            "signature entry {entry_index} packed mask offset overflows"
                        ))
                    })?,
                    entry_index,
                    "mask/rw_mask/stream",
                )?;
                (
                    (packed & 0xFF) as u8,
                    ((packed >> 8) & 0xFF) as u8,
                    ((packed >> 16) & 0xFF) as u32,
                )
            }
            SIGNATURE_ENTRY_LEN_V1 => {
                // 32-byte variant: mask/rw bytes followed by stream/min-precision DWORDs.
                let mask_offset = entry_start.checked_add(20).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "signature entry {entry_index} mask offset overflows"
                    ))
                })?;
                let read_write_mask_offset = entry_start.checked_add(21).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "signature entry {entry_index} read_write_mask offset overflows"
                    ))
                })?;
                // Bytes 22..23 are reserved/padding in the v1 layout. Some toolchains may still emit
                // the v0 (24-byte) entry encoding under a `*G1` chunk ID; in that case these bytes
                // hold the packed `stream`/`min_precision` fields. Treat non-zero values here as a
                // strong indicator that the table is actually v0, and force the outer parser to
                // retry with the v0 entry size.
                let reserved0_offset = entry_start.checked_add(22).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "signature entry {entry_index} reserved0 offset overflows"
                    ))
                })?;
                let reserved1_offset = entry_start.checked_add(23).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "signature entry {entry_index} reserved1 offset overflows"
                    ))
                })?;
                let stream_offset = entry_start.checked_add(24).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "signature entry {entry_index} stream offset overflows"
                    ))
                })?;

                let mask = *bytes.get(mask_offset).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "need 1 byte for entry {entry_index} mask at {}",
                        mask_offset
                    ))
                })?;
                let read_write_mask = *bytes.get(read_write_mask_offset).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "need 1 byte for entry {entry_index} read_write_mask at {}",
                        read_write_mask_offset
                    ))
                })?;
                let reserved0 = *bytes.get(reserved0_offset).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "need 1 byte for entry {entry_index} reserved0 at {}",
                        reserved0_offset
                    ))
                })?;
                let reserved1 = *bytes.get(reserved1_offset).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "need 1 byte for entry {entry_index} reserved1 at {}",
                        reserved1_offset
                    ))
                })?;
                if reserved0 != 0 || reserved1 != 0 {
                    return Err(DxbcError::invalid_chunk(format!(
                        "entry {entry_index} v1 layout reserved bytes are non-zero (reserved0=0x{reserved0:02x}, reserved1=0x{reserved1:02x})"
                    )));
                }
                let stream = read_u32_le_entry(bytes, stream_offset, entry_index, "stream")?;
                (mask, read_write_mask, stream)
            }
            other => {
                return Err(DxbcError::invalid_chunk(format!(
                    "unsupported signature entry size {other}"
                )))
            }
        };

        let semantic_name_str = read_cstring_entry(bytes, semantic_name_offset_usize, entry_index)?;
        let mut semantic_name = String::new();
        semantic_name
            .try_reserve_exact(semantic_name_str.len())
            .map_err(|_| {
                DxbcError::invalid_chunk(format!(
                    "entry {entry_index} semantic_name is too large to allocate"
                ))
            })?;
        semantic_name.push_str(semantic_name_str);

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

fn read_u32_le_entry(
    bytes: &[u8],
    offset: usize,
    entry_index: usize,
    field: &'static str,
) -> Result<u32, DxbcError> {
    read_u32_le(bytes, offset, field).map_err(|e| {
        DxbcError::invalid_chunk(format!("entry {entry_index} {field}: {}", e.context()))
    })
}

fn read_cstring_entry(bytes: &[u8], offset: usize, entry_index: usize) -> Result<&str, DxbcError> {
    read_cstring(bytes, offset, "semantic_name").map_err(|e| {
        DxbcError::invalid_chunk(format!(
            "entry {entry_index} semantic_name: {}",
            e.context()
        ))
    })
}

fn detect_v1_layout(bytes: &[u8], param_offset: usize) -> bool {
    // Heuristic: in the 32-byte entry layout the first entry stores `stream` and
    // `min_precision` as DWORDs. These values are typically small, whereas in the
    // 24-byte layout the same offsets usually point into the semantic name string
    // table, which starts with ASCII bytes (large u32 values).
    //
    // As a fast-path, prefer the v0 layout when the v0 packed-byte stream or
    // min-precision fields are non-zero. This avoids mis-detecting padded v0
    // signature tables as v1.
    let Some(stream_byte) = param_offset
        .checked_add(22)
        .and_then(|offset| bytes.get(offset))
        .copied()
    else {
        return false;
    };
    let Some(min_precision_byte) = param_offset
        .checked_add(23)
        .and_then(|offset| bytes.get(offset))
        .copied()
    else {
        return false;
    };
    if stream_byte != 0 || min_precision_byte != 0 {
        return false;
    }

    let Some(stream) = param_offset
        .checked_add(24)
        .and_then(|offset| read_u32_le_opt(bytes, offset))
    else {
        return false;
    };
    let Some(min_precision) = param_offset
        .checked_add(28)
        .and_then(|offset| read_u32_le_opt(bytes, offset))
    else {
        return false;
    };
    (stream <= 3) && (min_precision <= 8)
}

fn read_u32_le_opt(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = bytes.get(offset..end)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
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
