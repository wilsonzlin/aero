//! Parsers for DXBC resource definition chunks (`RDEF`).
//!
//! The `RDEF` chunk contains shader reflection information for D3D10+ shaders.
//! This module intentionally parses only a small subset needed by Aero today:
//! the list of bound resources and their binding points.
//!
//! The parser is designed for **untrusted** inputs: it validates all offsets and
//! sizes and never panics on malformed data.

use crate::DxbcError;

const RDEF_HEADER_LEN: usize = 28;
const RDEF_RESOURCE_ENTRY_LEN: usize = 32;

/// A single bound resource entry from an `RDEF` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBinding {
    /// Resource name as stored in the chunk string table.
    pub name: String,
    /// The binding point (e.g. `t3` has `bind_point == 3`).
    pub bind_point: u32,
    /// Number of contiguous binding points used by this resource.
    pub bind_count: u32,
    /// Resource type (`D3D_SHADER_INPUT_TYPE`) as a raw `u32`.
    pub ty: u32,
    /// Resource dimension (`D3D_SRV_DIMENSION`) as a raw `u32`.
    pub dimension: u32,
}

/// Minimal information extracted from an `RDEF` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDefs {
    /// Optional creator string (often the compiler version).
    pub creator: Option<String>,
    /// Bound resource entries.
    pub resources: Vec<ResourceBinding>,
}

/// Parses an `RDEF` chunk payload.
///
/// The input must be the chunk payload bytes (the data following the chunk's
/// FourCC and size inside the DXBC container).
pub fn parse_rdef_chunk(bytes: &[u8]) -> Result<ResourceDefs, DxbcError> {
    if bytes.len() < RDEF_HEADER_LEN {
        return Err(DxbcError::invalid_chunk(format!(
            "RDEF chunk is truncated: need {RDEF_HEADER_LEN} bytes for header, got {}",
            bytes.len()
        )));
    }

    let _cb_count = read_u32_le(bytes, 0, "cb_count")?;
    let _cb_offset = read_u32_le(bytes, 4, "cb_offset")?;
    let res_count = read_u32_le(bytes, 8, "resource_count")? as usize;
    let res_offset = read_u32_le(bytes, 12, "resource_offset")? as usize;
    let _shader_model = read_u32_le(bytes, 16, "shader_model")?;
    let _flags = read_u32_le(bytes, 20, "flags")?;
    let creator_offset = read_u32_le(bytes, 24, "creator_offset")? as usize;

    let creator = if creator_offset != 0 {
        Some(read_cstring_owned(bytes, creator_offset, "creator")?)
    } else {
        None
    };

    let table_bytes = res_count
        .checked_mul(RDEF_RESOURCE_ENTRY_LEN)
        .ok_or_else(|| DxbcError::invalid_chunk("resource_count overflows resource table size"))?;
    let table_end = res_offset.checked_add(table_bytes).ok_or_else(|| {
        DxbcError::invalid_chunk("resource_offset overflows when computing resource table end")
    })?;
    if table_end > bytes.len() {
        return Err(DxbcError::invalid_chunk(format!(
            "resource table at {res_offset}..{table_end} is outside chunk length {}",
            bytes.len()
        )));
    }

    let mut resources = Vec::new();
    resources
        .try_reserve_exact(res_count)
        .map_err(|_| DxbcError::invalid_chunk("resource table is too large to allocate"))?;

    for entry_index in 0..res_count {
        let entry_offset = entry_index.checked_mul(RDEF_RESOURCE_ENTRY_LEN).ok_or_else(|| {
            DxbcError::invalid_chunk(format!("resource entry {entry_index} offset overflows"))
        })?;
        let entry_start = res_offset.checked_add(entry_offset).ok_or_else(|| {
            DxbcError::invalid_chunk(format!("resource entry {entry_index} start overflows"))
        })?;

        let name_offset = read_u32_le_entry(bytes, entry_start, entry_index, "name_offset")?;
        let ty = read_u32_le_entry(
            bytes,
            entry_start
                .checked_add(4)
                .ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "resource entry {entry_index} type offset overflows"
                    ))
                })?,
            entry_index,
            "type",
        )?;
        let dimension = read_u32_le_entry(
            bytes,
            entry_start
                .checked_add(12)
                .ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "resource entry {entry_index} dimension offset overflows"
                    ))
                })?,
            entry_index,
            "dimension",
        )?;
        let bind_point = read_u32_le_entry(
            bytes,
            entry_start
                .checked_add(20)
                .ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "resource entry {entry_index} bind_point offset overflows"
                    ))
                })?,
            entry_index,
            "bind_point",
        )?;
        let bind_count = read_u32_le_entry(
            bytes,
            entry_start
                .checked_add(24)
                .ok_or_else(|| {
                    DxbcError::invalid_chunk(format!(
                        "resource entry {entry_index} bind_count offset overflows"
                    ))
                })?,
            entry_index,
            "bind_count",
        )?;

        let name_offset_usize = name_offset as usize;
        let name = read_cstring_owned_entry(bytes, name_offset_usize, entry_index, "name")?;
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

fn read_u32_le_entry(
    bytes: &[u8],
    offset: usize,
    entry_index: usize,
    field: &'static str,
) -> Result<u32, DxbcError> {
    read_u32_le(bytes, offset, field).map_err(|e| {
        DxbcError::invalid_chunk(format!("resource entry {entry_index} {field}: {}", e.context()))
    })
}

fn read_cstring_owned_entry(
    bytes: &[u8],
    offset: usize,
    entry_index: usize,
    field: &'static str,
) -> Result<String, DxbcError> {
    read_cstring_owned(bytes, offset, field).map_err(|e| {
        DxbcError::invalid_chunk(format!("resource entry {entry_index} {field}: {}", e.context()))
    })
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

fn read_cstring_owned(bytes: &[u8], offset: usize, what: &str) -> Result<String, DxbcError> {
    let s = read_cstring(bytes, offset, what)?;
    let mut out = String::new();
    out.try_reserve_exact(s.len()).map_err(|_| {
        DxbcError::invalid_chunk(format!("{what} string is too large to allocate"))
    })?;
    out.push_str(s);
    Ok(out)
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

