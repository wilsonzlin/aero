//! Parser for DXBC resource definition chunks (`RDEF`).
//!
//! The `RDEF` chunk (resource definition) contains reflection-style information
//! about:
//! - Constant buffers (size, variables, types)
//! - Resource bindings (SRV/UAV/sampler/cbuffer bindings, dimensions, counts)
//!
//! This module is designed for parsing **untrusted** blobs: all offsets and
//! lengths are validated and parsing never uses `unsafe`.

use crate::{DxbcError, FourCC};

const RDEF_HEADER_LEN_MIN: usize = 7 * 4; // 7 DWORDs (cb count/offset, rb count/offset, target, flags, creator_offset)
const CB_DESC_LEN: usize = 24;
const VAR_DESC_LEN: usize = 24;
const TYPE_DESC_LEN: usize = 16;
const MEMBER_DESC_LEN: usize = 8;
const RESOURCE_BIND_DESC_LEN: usize = 32;

/// Parsed contents of an `RDEF` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdefChunk {
    /// Target version token stored in the `RDEF` header.
    pub target: u32,
    /// Compile flags stored in the `RDEF` header.
    pub flags: u32,
    /// Optional creator string (often the compiler/toolchain name).
    pub creator: Option<String>,
    /// Constant buffer descriptions.
    pub constant_buffers: Vec<RdefConstantBuffer>,
    /// Bound resource descriptions (SRV/UAV/samplers/cbuffers, etc.).
    pub bound_resources: Vec<RdefResourceBinding>,
}

/// A constant buffer as described by `RDEF`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdefConstantBuffer {
    /// Name of the constant buffer (e.g. `"$Globals"` or `"Globals"`).
    pub name: String,
    /// Optional binding slot (`b#` / bind point) for this constant buffer.
    ///
    /// This is derived by matching the constant buffer's name against the
    /// resource binding table (entries with input type `D3D_SIT_CBUFFER` /
    /// `D3D_SIT_TBUFFER`).
    pub bind_point: Option<u32>,
    /// Optional number of bound slots (`bind_count`) for this buffer.
    pub bind_count: Option<u32>,
    /// Size, in bytes, of the constant buffer.
    pub size: u32,
    /// Declared variables within the constant buffer.
    pub variables: Vec<RdefVariable>,
}

/// A single variable within a constant buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdefVariable {
    /// Variable name.
    pub name: String,
    /// Byte offset from the start of the constant buffer.
    pub offset: u32,
    /// Size, in bytes, of the variable.
    pub size: u32,
    /// Variable flags (raw `u32`).
    pub flags: u32,
    /// Type information for the variable.
    pub ty: RdefType,
}

/// Type information for a constant buffer variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdefType {
    /// Variable class (`D3D_SHADER_VARIABLE_CLASS`), stored as a raw `u16`.
    pub class: u16,
    /// Variable type (`D3D_SHADER_VARIABLE_TYPE`), stored as a raw `u16`.
    pub ty: u16,
    /// Row count (e.g. 4 for `float4x4`).
    pub rows: u16,
    /// Column count (e.g. 4 for `float4x4`).
    pub columns: u16,
    /// Array element count (0 or 1 for non-arrays; stored as raw `u16`).
    pub elements: u16,
    /// Struct members (empty for non-struct types).
    pub members: Vec<RdefStructMember>,
}

/// A struct member within a [`RdefType`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdefStructMember {
    /// Member name.
    pub name: String,
    /// Member type.
    pub ty: RdefType,
}

/// A single resource binding entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdefResourceBinding {
    /// Resource name.
    pub name: String,
    /// Input type (`D3D_SHADER_INPUT_TYPE`), stored as raw `u32`.
    pub input_type: u32,
    /// Return type (`D3D_RESOURCE_RETURN_TYPE`), stored as raw `u32`.
    pub return_type: u32,
    /// Resource dimension (`D3D_SRV_DIMENSION`), stored as raw `u32`.
    pub dimension: u32,
    /// Sample count (raw `u32`, often 0 or 1).
    pub sample_count: u32,
    /// Bind point (slot).
    pub bind_point: u32,
    /// Bind count.
    pub bind_count: u32,
    /// Resource flags (`D3D_SHADER_INPUT_FLAGS`), stored as raw `u32`.
    pub flags: u32,
}

/// Parses an `RDEF` chunk payload.
pub fn parse_rdef_chunk(bytes: &[u8]) -> Result<RdefChunk, DxbcError> {
    parse_rdef_chunk_impl(None, bytes)
}

/// Parses an `RDEF`-like chunk payload, using the chunk `fourcc` for diagnostics.
///
/// This exists because some toolchains use slightly different chunk IDs for
/// reflection data; the parser itself is tolerant as long as the payload uses
/// the standard `RDEF` layout.
pub fn parse_rdef_chunk_with_fourcc(fourcc: FourCC, bytes: &[u8]) -> Result<RdefChunk, DxbcError> {
    parse_rdef_chunk_impl(Some(fourcc), bytes)
}

/// Parses an `RDEF`-like chunk payload (kept for parity with other parsers).
pub fn parse_rdef_chunk_for_fourcc(fourcc: FourCC, bytes: &[u8]) -> Result<RdefChunk, DxbcError> {
    parse_rdef_chunk_with_fourcc(fourcc, bytes)
}

fn parse_rdef_chunk_impl(fourcc: Option<FourCC>, bytes: &[u8]) -> Result<RdefChunk, DxbcError> {
    if bytes.len() < RDEF_HEADER_LEN_MIN {
        return Err(DxbcError::invalid_chunk(format!(
            "{} header is truncated: need at least {RDEF_HEADER_LEN_MIN} bytes, got {}",
            fourcc.unwrap_or(FourCC(*b"RDEF")),
            bytes.len()
        )));
    }

    let cb_count = read_u32_le(bytes, 0, "cb_count")?;
    let cb_offset = read_u32_le(bytes, 4, "cb_offset")?;
    let rb_count = read_u32_le(bytes, 8, "resource_count")?;
    let rb_offset = read_u32_le(bytes, 12, "resource_offset")?;
    let target = read_u32_le(bytes, 16, "target")?;
    let flags = read_u32_le(bytes, 20, "flags")?;
    let creator_offset = read_u32_le(bytes, 24, "creator_offset")?;

    let creator = if creator_offset == 0 {
        None
    } else {
        Some(read_cstring(bytes, creator_offset as usize, "creator")?.to_owned())
    };

    let bound_resources = parse_bound_resources(bytes, rb_offset, rb_count)?;
    let mut constant_buffers = parse_constant_buffers(bytes, cb_offset, cb_count)?;

    // Link constant buffers to their binding slots via the resource table.
    for cb in constant_buffers.iter_mut() {
        // D3D_SIT_CBUFFER = 0, D3D_SIT_TBUFFER = 1.
        let binding = bound_resources
            .iter()
            .find(|res| (res.input_type == 0 || res.input_type == 1) && res.name == cb.name);
        if let Some(res) = binding {
            cb.bind_point = Some(res.bind_point);
            cb.bind_count = Some(res.bind_count);
        }
    }

    Ok(RdefChunk {
        target,
        flags,
        creator,
        constant_buffers,
        bound_resources,
    })
}

fn parse_constant_buffers(
    bytes: &[u8],
    cb_offset: u32,
    cb_count: u32,
) -> Result<Vec<RdefConstantBuffer>, DxbcError> {
    let cb_count_usize = cb_count as usize;
    let cb_offset_usize = cb_offset as usize;

    if cb_count_usize == 0 {
        if cb_offset_usize > bytes.len() {
            return Err(DxbcError::invalid_chunk(format!(
                "RDEF cb_offset {cb_offset} is outside chunk length {}",
                bytes.len()
            )));
        }
        return Ok(Vec::new());
    }

    let table_bytes = cb_count_usize.checked_mul(CB_DESC_LEN).ok_or_else(|| {
        DxbcError::invalid_chunk("RDEF constant buffer count overflows table size")
    })?;
    let table_end = cb_offset_usize
        .checked_add(table_bytes)
        .ok_or_else(|| DxbcError::invalid_chunk("RDEF constant buffer table end overflows"))?;
    if table_end > bytes.len() {
        return Err(DxbcError::invalid_chunk(format!(
            "RDEF constant buffer table at {cb_offset_usize}..{table_end} is outside chunk length {}",
            bytes.len()
        )));
    }

    let mut out = Vec::new();
    out.try_reserve_exact(cb_count_usize).map_err(|_| {
        DxbcError::invalid_chunk(format!(
            "RDEF constant buffer count {cb_count} is too large to allocate"
        ))
    })?;

    for i in 0..cb_count_usize {
        let entry_start = cb_offset_usize
            .checked_add(i.checked_mul(CB_DESC_LEN).ok_or_else(|| {
                DxbcError::invalid_chunk(format!("RDEF cbuffer[{i}] entry offset overflows"))
            })?)
            .ok_or_else(|| {
                DxbcError::invalid_chunk(format!("RDEF cbuffer[{i}] entry start overflows"))
            })?;

        let name_offset = read_u32_le_entry(bytes, entry_start, "cbuffer", i, "name_offset")?;
        let var_count = read_u32_le_entry(bytes, entry_start + 4, "cbuffer", i, "var_count")?;
        let var_offset = read_u32_le_entry(bytes, entry_start + 8, "cbuffer", i, "var_offset")?;
        let size = read_u32_le_entry(bytes, entry_start + 12, "cbuffer", i, "size")?;

        // Flags/type exist in the encoding but are not currently surfaced.
        let _flags = read_u32_le_entry(bytes, entry_start + 16, "cbuffer", i, "flags")?;
        let _cb_type = read_u32_le_entry(bytes, entry_start + 20, "cbuffer", i, "cb_type")?;

        let name = read_cstring_entry(bytes, name_offset as usize, "cbuffer", i, "name")?;
        let variables = parse_variables(bytes, var_offset, var_count, i)?;

        out.push(RdefConstantBuffer {
            name: name.to_owned(),
            bind_point: None,
            bind_count: None,
            size,
            variables,
        });
    }

    Ok(out)
}

fn parse_variables(
    bytes: &[u8],
    var_offset: u32,
    var_count: u32,
    cbuffer_index: usize,
) -> Result<Vec<RdefVariable>, DxbcError> {
    let var_count_usize = var_count as usize;
    let var_offset_usize = var_offset as usize;

    if var_count_usize == 0 {
        if var_offset_usize > bytes.len() {
            return Err(DxbcError::invalid_chunk(format!(
                "RDEF cbuffer[{cbuffer_index}] var_offset {var_offset} is outside chunk length {}",
                bytes.len()
            )));
        }
        return Ok(Vec::new());
    }

    let table_bytes = var_count_usize.checked_mul(VAR_DESC_LEN).ok_or_else(|| {
        DxbcError::invalid_chunk(format!(
            "RDEF cbuffer[{cbuffer_index}] variable count overflows table size"
        ))
    })?;
    let table_end = var_offset_usize.checked_add(table_bytes).ok_or_else(|| {
        DxbcError::invalid_chunk(format!(
            "RDEF cbuffer[{cbuffer_index}] variable table end overflows"
        ))
    })?;
    if table_end > bytes.len() {
        return Err(DxbcError::invalid_chunk(format!(
            "RDEF cbuffer[{cbuffer_index}] variable table at {var_offset_usize}..{table_end} is outside chunk length {}",
            bytes.len()
        )));
    }

    let mut out = Vec::new();
    out.try_reserve_exact(var_count_usize).map_err(|_| {
        DxbcError::invalid_chunk(format!(
            "RDEF cbuffer[{cbuffer_index}] variable count {var_count} is too large to allocate"
        ))
    })?;

    for i in 0..var_count_usize {
        let entry_start = var_offset_usize
            .checked_add(i.checked_mul(VAR_DESC_LEN).ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "RDEF cbuffer[{cbuffer_index}] var[{i}] entry offset overflows"
                ))
            })?)
            .ok_or_else(|| {
                DxbcError::invalid_chunk(format!(
                    "RDEF cbuffer[{cbuffer_index}] var[{i}] entry start overflows"
                ))
            })?;

        let name_offset =
            read_u32_le_entry(bytes, entry_start, "var", i, "name_offset").map_err(|e| {
                DxbcError::invalid_chunk(format!("RDEF cbuffer[{cbuffer_index}] {}", e.context()))
            })?;
        let start_offset = read_u32_le_entry(bytes, entry_start + 4, "var", i, "start_offset")
            .map_err(|e| {
                DxbcError::invalid_chunk(format!("RDEF cbuffer[{cbuffer_index}] {}", e.context()))
            })?;
        let size = read_u32_le_entry(bytes, entry_start + 8, "var", i, "size").map_err(|e| {
            DxbcError::invalid_chunk(format!("RDEF cbuffer[{cbuffer_index}] {}", e.context()))
        })?;
        let flags = read_u32_le_entry(bytes, entry_start + 12, "var", i, "flags").map_err(|e| {
            DxbcError::invalid_chunk(format!("RDEF cbuffer[{cbuffer_index}] {}", e.context()))
        })?;
        let type_offset = read_u32_le_entry(bytes, entry_start + 16, "var", i, "type_offset")
            .map_err(|e| {
                DxbcError::invalid_chunk(format!("RDEF cbuffer[{cbuffer_index}] {}", e.context()))
            })?;
        let _default_value_offset =
            read_u32_le_entry(bytes, entry_start + 20, "var", i, "default_value_offset").map_err(
                |e| {
                    DxbcError::invalid_chunk(format!(
                        "RDEF cbuffer[{cbuffer_index}] {}",
                        e.context()
                    ))
                },
            )?;

        let name =
            read_cstring_entry(bytes, name_offset as usize, "var", i, "name").map_err(|e| {
                DxbcError::invalid_chunk(format!("RDEF cbuffer[{cbuffer_index}] {}", e.context()))
            })?;
        let ty = parse_type(bytes, type_offset as usize, 0).map_err(|e| {
            DxbcError::invalid_chunk(format!(
                "RDEF cbuffer[{cbuffer_index}] var[{i}] type: {}",
                e.context()
            ))
        })?;

        out.push(RdefVariable {
            name: name.to_owned(),
            offset: start_offset,
            size,
            flags,
            ty,
        });
    }

    Ok(out)
}

fn parse_type(bytes: &[u8], offset: usize, depth: u32) -> Result<RdefType, DxbcError> {
    if depth > 32 {
        return Err(DxbcError::invalid_chunk("RDEF type recursion too deep"));
    }
    let end = offset.checked_add(TYPE_DESC_LEN).ok_or_else(|| {
        DxbcError::invalid_chunk("RDEF type offset overflows when reading type desc")
    })?;
    let slice = bytes.get(offset..end).ok_or_else(|| {
        DxbcError::invalid_chunk(format!(
            "RDEF type desc at {offset}..{end} is outside chunk length {}",
            bytes.len()
        ))
    })?;

    let class = u16::from_le_bytes([slice[0], slice[1]]);
    let ty = u16::from_le_bytes([slice[2], slice[3]]);
    let rows = u16::from_le_bytes([slice[4], slice[5]]);
    let columns = u16::from_le_bytes([slice[6], slice[7]]);
    let elements = u16::from_le_bytes([slice[8], slice[9]]);
    let member_count = u16::from_le_bytes([slice[10], slice[11]]);
    let member_offset = u32::from_le_bytes([slice[12], slice[13], slice[14], slice[15]]) as usize;

    let member_count_usize = member_count as usize;
    let mut members = Vec::new();
    if member_count_usize > 0 {
        let table_bytes = member_count_usize
            .checked_mul(MEMBER_DESC_LEN)
            .ok_or_else(|| DxbcError::invalid_chunk("RDEF member count overflows table size"))?;
        let table_end = member_offset
            .checked_add(table_bytes)
            .ok_or_else(|| DxbcError::invalid_chunk("RDEF member table end overflows"))?;
        if table_end > bytes.len() {
            return Err(DxbcError::invalid_chunk(format!(
                "RDEF member table at {member_offset}..{table_end} is outside chunk length {}",
                bytes.len()
            )));
        }

        members.try_reserve_exact(member_count_usize).map_err(|_| {
            DxbcError::invalid_chunk(format!(
                "RDEF member count {member_count} is too large to allocate"
            ))
        })?;

        for i in 0..member_count_usize {
            let entry_start = member_offset
                .checked_add(i.checked_mul(MEMBER_DESC_LEN).ok_or_else(|| {
                    DxbcError::invalid_chunk(format!("RDEF member[{i}] entry offset overflows"))
                })?)
                .ok_or_else(|| {
                    DxbcError::invalid_chunk(format!("RDEF member[{i}] entry start overflows"))
                })?;

            let name_offset = read_u32_le(bytes, entry_start, "member name_offset")? as usize;
            let ty_offset = read_u32_le(bytes, entry_start + 4, "member type_offset")? as usize;

            let name = read_cstring(bytes, name_offset, "member name")?;
            let ty = parse_type(bytes, ty_offset, depth + 1)?;
            members.push(RdefStructMember {
                name: name.to_owned(),
                ty,
            });
        }
    }

    Ok(RdefType {
        class,
        ty,
        rows,
        columns,
        elements,
        members,
    })
}

fn parse_bound_resources(
    bytes: &[u8],
    rb_offset: u32,
    rb_count: u32,
) -> Result<Vec<RdefResourceBinding>, DxbcError> {
    let rb_count_usize = rb_count as usize;
    let rb_offset_usize = rb_offset as usize;

    if rb_count_usize == 0 {
        if rb_offset_usize > bytes.len() {
            return Err(DxbcError::invalid_chunk(format!(
                "RDEF resource_offset {rb_offset} is outside chunk length {}",
                bytes.len()
            )));
        }
        return Ok(Vec::new());
    }

    let table_bytes = rb_count_usize
        .checked_mul(RESOURCE_BIND_DESC_LEN)
        .ok_or_else(|| DxbcError::invalid_chunk("RDEF resource count overflows table size"))?;
    let table_end = rb_offset_usize
        .checked_add(table_bytes)
        .ok_or_else(|| DxbcError::invalid_chunk("RDEF resource table end overflows"))?;
    if table_end > bytes.len() {
        return Err(DxbcError::invalid_chunk(format!(
            "RDEF resource table at {rb_offset_usize}..{table_end} is outside chunk length {}",
            bytes.len()
        )));
    }

    let mut out = Vec::new();
    out.try_reserve_exact(rb_count_usize).map_err(|_| {
        DxbcError::invalid_chunk(format!(
            "RDEF resource count {rb_count} is too large to allocate"
        ))
    })?;

    for i in 0..rb_count_usize {
        let entry_start = rb_offset_usize
            .checked_add(i.checked_mul(RESOURCE_BIND_DESC_LEN).ok_or_else(|| {
                DxbcError::invalid_chunk(format!("RDEF resource[{i}] entry offset overflows"))
            })?)
            .ok_or_else(|| {
                DxbcError::invalid_chunk(format!("RDEF resource[{i}] entry start overflows"))
            })?;

        let name_offset = read_u32_le_entry(bytes, entry_start, "resource", i, "name_offset")?;
        let input_type = read_u32_le_entry(bytes, entry_start + 4, "resource", i, "input_type")?;
        let return_type = read_u32_le_entry(bytes, entry_start + 8, "resource", i, "return_type")?;
        let dimension = read_u32_le_entry(bytes, entry_start + 12, "resource", i, "dimension")?;
        let sample_count =
            read_u32_le_entry(bytes, entry_start + 16, "resource", i, "sample_count")?;
        let bind_point = read_u32_le_entry(bytes, entry_start + 20, "resource", i, "bind_point")?;
        let bind_count = read_u32_le_entry(bytes, entry_start + 24, "resource", i, "bind_count")?;
        let flags = read_u32_le_entry(bytes, entry_start + 28, "resource", i, "flags")?;

        let name = read_cstring_entry(bytes, name_offset as usize, "resource", i, "name")?;

        out.push(RdefResourceBinding {
            name: name.to_owned(),
            input_type,
            return_type,
            dimension,
            sample_count,
            bind_point,
            bind_count,
            flags,
        });
    }

    Ok(out)
}

fn read_u32_le_entry(
    bytes: &[u8],
    offset: usize,
    table: &'static str,
    index: usize,
    field: &'static str,
) -> Result<u32, DxbcError> {
    read_u32_le(bytes, offset, field)
        .map_err(|e| DxbcError::invalid_chunk(format!("{table}[{index}] {field}: {}", e.context())))
}

fn read_cstring_entry<'a>(
    bytes: &'a [u8],
    offset: usize,
    table: &'static str,
    index: usize,
    field: &'static str,
) -> Result<&'a str, DxbcError> {
    read_cstring(bytes, offset, field)
        .map_err(|e| DxbcError::invalid_chunk(format!("{table}[{index}] {field}: {}", e.context())))
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
