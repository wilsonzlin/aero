use super::byte_reader::ByteReader;
use super::{DxbcError, FourCc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcReflection {
    pub target: u32,
    pub flags: u32,
    pub creator: Option<String>,
    pub constant_buffers: Vec<DxbcConstantBuffer>,
    pub resources: Vec<DxbcResourceBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcConstantBuffer {
    pub name: String,
    pub size: u32,
    pub variables: Vec<DxbcVariable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcVariable {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    pub flags: u32,
    pub ty: DxbcType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcType {
    pub class: u16,
    pub ty: u16,
    pub rows: u16,
    pub columns: u16,
    pub elements: u16,
    pub members: Vec<DxbcStructMember>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcStructMember {
    pub name: String,
    pub ty: DxbcType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcResourceBinding {
    pub name: String,
    pub input_type: u32,
    pub bind_point: u32,
    pub bind_count: u32,
    pub flags: u32,
}

pub(crate) fn parse_rdef(bytes: &[u8]) -> Result<DxbcReflection, DxbcError> {
    let mut r = ByteReader::new(bytes);

    // Most DXBCs emit an 8-dword header. Older targets may omit the final dword; accept either.
    let cb_count = r.read_u32_le()?;
    let cb_offset = r.read_u32_le()?;
    let rb_count = r.read_u32_le()?;
    let rb_offset = r.read_u32_le()?;
    let target = r.read_u32_le()?;
    let flags = r.read_u32_le()?;
    let creator_offset = r.read_u32_le()?;
    let _interface_slots = if r.remaining() >= 4 {
        Some(r.read_u32_le()?)
    } else {
        None
    };

    let creator = (creator_offset != 0)
        .then(|| {
            r.read_cstring_at(creator_offset as usize)
                .map(|s| s.to_owned())
        })
        .transpose()?;

    let constant_buffers = parse_constant_buffers(bytes, cb_offset, cb_count)?;
    let resources = parse_bound_resources(bytes, rb_offset, rb_count)?;

    Ok(DxbcReflection {
        target,
        flags,
        creator,
        constant_buffers,
        resources,
    })
}

fn parse_constant_buffers(
    bytes: &[u8],
    cb_offset: u32,
    cb_count: u32,
) -> Result<Vec<DxbcConstantBuffer>, DxbcError> {
    let mut out = Vec::with_capacity(cb_count as usize);
    if cb_count == 0 {
        return Ok(out);
    }

    let cb_offset = cb_offset as usize;
    let desc_size = 24usize;
    let total = (cb_count as usize)
        .checked_mul(desc_size)
        .ok_or(DxbcError::InvalidChunk {
            fourcc: FourCc::from_str("RDEF"),
            reason: "constant buffer count overflow",
        })?;

    if cb_offset.checked_add(total).is_none() || cb_offset + total > bytes.len() {
        return Err(DxbcError::InvalidChunk {
            fourcc: FourCc::from_str("RDEF"),
            reason: "constant buffer table out of bounds",
        });
    }

    let r = ByteReader::new(bytes);
    for i in 0..cb_count {
        let base = cb_offset + (i as usize) * desc_size;
        let mut cr = r.fork(base)?;
        let name_offset = cr.read_u32_le()?;
        let var_count = cr.read_u32_le()?;
        let var_offset = cr.read_u32_le()?;
        let size = cr.read_u32_le()?;
        let _cb_flags = cr.read_u32_le()?;
        let _cb_type = cr.read_u32_le()?;

        let name = r.read_cstring_at(name_offset as usize)?.to_owned();
        let variables = parse_variables(bytes, var_offset, var_count)?;

        out.push(DxbcConstantBuffer {
            name,
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
) -> Result<Vec<DxbcVariable>, DxbcError> {
    let mut out = Vec::with_capacity(var_count as usize);
    if var_count == 0 {
        return Ok(out);
    }

    let var_offset = var_offset as usize;
    let desc_size = 24usize;
    let total = (var_count as usize)
        .checked_mul(desc_size)
        .ok_or(DxbcError::InvalidChunk {
            fourcc: FourCc::from_str("RDEF"),
            reason: "variable count overflow",
        })?;

    if var_offset.checked_add(total).is_none() || var_offset + total > bytes.len() {
        return Err(DxbcError::InvalidChunk {
            fourcc: FourCc::from_str("RDEF"),
            reason: "variable table out of bounds",
        });
    }

    let r = ByteReader::new(bytes);
    for i in 0..var_count {
        let base = var_offset + (i as usize) * desc_size;
        let mut vr = r.fork(base)?;
        let name_offset = vr.read_u32_le()?;
        let start_offset = vr.read_u32_le()?;
        let size = vr.read_u32_le()?;
        let flags = vr.read_u32_le()?;
        let type_offset = vr.read_u32_le()?;
        let _default_value_offset = vr.read_u32_le()?;

        let name = r.read_cstring_at(name_offset as usize)?.to_owned();
        let ty = parse_type(bytes, type_offset as usize, 0)?;

        out.push(DxbcVariable {
            name,
            offset: start_offset,
            size,
            flags,
            ty,
        });
    }

    Ok(out)
}

fn parse_type(bytes: &[u8], offset: usize, depth: u32) -> Result<DxbcType, DxbcError> {
    if depth > 32 {
        return Err(DxbcError::InvalidChunk {
            fourcc: FourCc::from_str("RDEF"),
            reason: "type recursion too deep",
        });
    }

    let mut r = ByteReader::new(bytes).fork(offset)?;

    let class = r.read_u16_le()?;
    let ty = r.read_u16_le()?;
    let rows = r.read_u16_le()?;
    let columns = r.read_u16_le()?;
    let elements = r.read_u16_le()?;
    let member_count = r.read_u16_le()?;
    let member_offset = r.read_u32_le()?;

    let mut members = Vec::with_capacity(member_count as usize);
    if member_count > 0 {
        let mo = member_offset as usize;
        let member_desc_size = 8usize;
        let total = (member_count as usize)
            .checked_mul(member_desc_size)
            .ok_or(DxbcError::InvalidChunk {
                fourcc: FourCc::from_str("RDEF"),
                reason: "member count overflow",
            })?;

        if mo.checked_add(total).is_none() || mo + total > bytes.len() {
            return Err(DxbcError::InvalidChunk {
                fourcc: FourCc::from_str("RDEF"),
                reason: "member table out of bounds",
            });
        }

        let br = ByteReader::new(bytes);
        for i in 0..member_count {
            let base = mo + (i as usize) * member_desc_size;
            let mut mr = br.fork(base)?;
            let name_offset = mr.read_u32_le()?;
            let ty_offset = mr.read_u32_le()?;

            let name = br.read_cstring_at(name_offset as usize)?.to_owned();
            let member_ty = parse_type(bytes, ty_offset as usize, depth + 1)?;
            members.push(DxbcStructMember {
                name,
                ty: member_ty,
            });
        }
    }

    Ok(DxbcType {
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
) -> Result<Vec<DxbcResourceBinding>, DxbcError> {
    let mut out = Vec::with_capacity(rb_count as usize);
    if rb_count == 0 {
        return Ok(out);
    }

    let rb_offset = rb_offset as usize;
    let desc_size = 32usize;
    let total = (rb_count as usize)
        .checked_mul(desc_size)
        .ok_or(DxbcError::InvalidChunk {
            fourcc: FourCc::from_str("RDEF"),
            reason: "resource count overflow",
        })?;

    if rb_offset.checked_add(total).is_none() || rb_offset + total > bytes.len() {
        return Err(DxbcError::InvalidChunk {
            fourcc: FourCc::from_str("RDEF"),
            reason: "resource table out of bounds",
        });
    }

    let r = ByteReader::new(bytes);
    for i in 0..rb_count {
        let base = rb_offset + (i as usize) * desc_size;
        let mut rr = r.fork(base)?;

        let name_offset = rr.read_u32_le()?;
        let input_type = rr.read_u32_le()?;
        let _return_type = rr.read_u32_le()?;
        let _dimension = rr.read_u32_le()?;
        let _sample_count = rr.read_u32_le()?;
        let bind_point = rr.read_u32_le()?;
        let bind_count = rr.read_u32_le()?;
        let res_flags = rr.read_u32_le()?;

        let name = r.read_cstring_at(name_offset as usize)?.to_owned();
        out.push(DxbcResourceBinding {
            name,
            input_type,
            bind_point,
            bind_count,
            flags: res_flags,
        });
    }

    Ok(out)
}
