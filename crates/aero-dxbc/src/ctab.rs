//! Parsers for legacy Direct3D constant table chunks (`CTAB`).
//!
//! The `CTAB` chunk format originates from older shader models and is sometimes
//! embedded in DXBC containers for debugging/reflection purposes. The full
//! format is complex; this module provides a minimal parser that extracts:
//!
//! - Optional `creator` and `target` strings.
//! - A list of constants and their register ranges.
//!
//! The parser is designed for **untrusted** inputs: it validates all offsets and
//! sizes and never panics on malformed data.

use crate::DxbcError;

const CTAB_HEADER_LEN: usize = 28;
const CTAB_CONSTANT_INFO_LEN: usize = 20;

/// A single constant entry from a `CTAB` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtabConstant {
    /// Constant name.
    pub name: String,
    /// Starting register index.
    pub register_index: u16,
    /// Number of registers used by this constant.
    pub register_count: u16,
}

/// Minimal information extracted from a `CTAB` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantTable {
    /// Optional creator string.
    pub creator: Option<String>,
    /// Optional shader target string (e.g. `"ps_2_0"`).
    pub target: Option<String>,
    /// Parsed constant entries.
    pub constants: Vec<CtabConstant>,
}

/// Parses a `CTAB` chunk payload.
///
/// The input must be the chunk payload bytes (the data following the chunk's
/// FourCC and size inside the DXBC container).
pub fn parse_ctab_chunk(bytes: &[u8]) -> Result<ConstantTable, DxbcError> {
    if bytes.len() < CTAB_HEADER_LEN {
        return Err(DxbcError::invalid_chunk(format!(
            "CTAB chunk is truncated: need {CTAB_HEADER_LEN} bytes for header, got {}",
            bytes.len()
        )));
    }

    let _size = read_u32_le(bytes, 0, "size")?;
    let creator_offset = read_u32_le(bytes, 4, "creator_offset")? as usize;
    let _version = read_u32_le(bytes, 8, "version")?;
    let constant_count = read_u32_le(bytes, 12, "constant_count")? as usize;
    let constant_offset = read_u32_le(bytes, 16, "constant_offset")? as usize;
    let _flags = read_u32_le(bytes, 20, "flags")?;
    let target_offset = read_u32_le(bytes, 24, "target_offset")? as usize;

    let creator = if creator_offset != 0 {
        Some(read_cstring_owned(bytes, creator_offset, "creator")?)
    } else {
        None
    };
    let target = if target_offset != 0 {
        Some(read_cstring_owned(bytes, target_offset, "target")?)
    } else {
        None
    };

    let table_bytes = constant_count
        .checked_mul(CTAB_CONSTANT_INFO_LEN)
        .ok_or_else(|| DxbcError::invalid_chunk("constant_count overflows constant table size"))?;
    let table_end = constant_offset.checked_add(table_bytes).ok_or_else(|| {
        DxbcError::invalid_chunk("constant_offset overflows when computing constant table end")
    })?;
    if table_end > bytes.len() {
        return Err(DxbcError::invalid_chunk(format!(
            "constant table at {constant_offset}..{table_end} is outside chunk length {}",
            bytes.len()
        )));
    }

    let mut constants = Vec::new();
    constants
        .try_reserve_exact(constant_count)
        .map_err(|_| DxbcError::invalid_chunk("constant table is too large to allocate"))?;

    for entry_index in 0..constant_count {
        let entry_offset = entry_index
            .checked_mul(CTAB_CONSTANT_INFO_LEN)
            .ok_or_else(|| {
                DxbcError::invalid_chunk(format!("constant entry {entry_index} offset overflows"))
            })?;
        let entry_start = constant_offset.checked_add(entry_offset).ok_or_else(|| {
            DxbcError::invalid_chunk(format!("constant entry {entry_index} start overflows"))
        })?;

        let name_offset = read_u32_le_entry(bytes, entry_start, entry_index, "name_offset")?;

        let register_index = read_u16_le_entry(
            bytes,
            entry_start.checked_add(6).ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "constant entry {entry_index} register_index offset overflows"
                ))
            })?,
            entry_index,
            "register_index",
        )?;
        let register_count = read_u16_le_entry(
            bytes,
            entry_start.checked_add(8).ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "constant entry {entry_index} register_count offset overflows"
                ))
            })?,
            entry_index,
            "register_count",
        )?;

        let name = read_cstring_owned_entry(bytes, name_offset as usize, entry_index, "name")?;
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

fn read_u32_le_entry(
    bytes: &[u8],
    offset: usize,
    entry_index: usize,
    field: &'static str,
) -> Result<u32, DxbcError> {
    read_u32_le(bytes, offset, field).map_err(|e| {
        DxbcError::invalid_chunk(format!(
            "constant entry {entry_index} {field}: {}",
            e.context()
        ))
    })
}

fn read_u16_le_entry(
    bytes: &[u8],
    offset: usize,
    entry_index: usize,
    field: &'static str,
) -> Result<u16, DxbcError> {
    read_u16_le(bytes, offset, field).map_err(|e| {
        DxbcError::invalid_chunk(format!(
            "constant entry {entry_index} {field}: {}",
            e.context()
        ))
    })
}

fn read_cstring_owned_entry(
    bytes: &[u8],
    offset: usize,
    entry_index: usize,
    field: &'static str,
) -> Result<String, DxbcError> {
    read_cstring_owned(bytes, offset, field).map_err(|e| {
        DxbcError::invalid_chunk(format!(
            "constant entry {entry_index} {field}: {}",
            e.context()
        ))
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

fn read_u16_le(bytes: &[u8], offset: usize, what: &str) -> Result<u16, DxbcError> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| DxbcError::invalid_chunk(format!("{what} offset overflows")))?;
    let slice = bytes.get(offset..end).ok_or_else(|| {
        DxbcError::invalid_chunk(format!(
            "need 2 bytes for {what} at {offset}..{end}, but chunk length is {}",
            bytes.len()
        ))
    })?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_cstring_owned(bytes: &[u8], offset: usize, what: &str) -> Result<String, DxbcError> {
    let s = read_cstring(bytes, offset, what)?;
    let mut out = String::new();
    out.try_reserve_exact(s.len())
        .map_err(|_| DxbcError::invalid_chunk(format!("{what} string is too large to allocate")))?;
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
