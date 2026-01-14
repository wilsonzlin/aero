use super::byte_reader::ByteReader;
use super::{DxbcError, FourCc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcSignature {
    pub parameters: Vec<DxbcSignatureParameter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcSignatureParameter {
    pub semantic_name: String,
    pub semantic_index: u32,
    pub register: u32,
    pub mask: u8,
    pub read_write_mask: u8,
}

pub(crate) fn parse_signature(fourcc: FourCc, bytes: &[u8]) -> Result<DxbcSignature, DxbcError> {
    let mut r = ByteReader::new(bytes);

    let param_count = r.read_u32_le()?;
    let param_offset = r.read_u32_le()?;

    // Support both common D3D10+ signature entry layouts:
    // - v0: 24-byte entries (`*SGN`, `PCSG`)
    // - v1: 32-byte entries (`*SG1`, `PCG1`)
    //
    // `aero-dxbc`'s non-robust parser also supports a heuristic for unknown FourCCs, but for the
    // legacy robust module keep the mapping simple and FourCC-driven.
    let entry_size = if fourcc.as_bytes()[3] == b'1' {
        32usize
    } else {
        24usize
    };
    let table_bytes =
        (param_count as usize)
            .checked_mul(entry_size)
            .ok_or(DxbcError::InvalidChunk {
                fourcc,
                reason: "parameter count overflow",
            })?;

    let table_start = param_offset as usize;
    if table_start.checked_add(table_bytes).is_none() || table_start + table_bytes > bytes.len() {
        return Err(DxbcError::InvalidChunk {
            fourcc,
            reason: "signature table out of bounds",
        });
    }

    let base = ByteReader::new(bytes);
    let mut parameters = Vec::with_capacity(param_count as usize);
    for i in 0..param_count {
        let offset = table_start + (i as usize) * entry_size;
        let mut pr = base.fork(offset)?;

        let semantic_name_offset = pr.read_u32_le()?;
        let semantic_index = pr.read_u32_le()?;
        let _system_value_type = pr.read_u32_le()?;
        let _component_type = pr.read_u32_le()?;
        let register = pr.read_u32_le()?;
        let mask = pr.read_u8()?;
        let read_write_mask = pr.read_u8()?;
        let _stream = pr.read_u8()?;
        let _min_precision = pr.read_u8()?;

        let semantic_name = base
            .read_cstring_at(semantic_name_offset as usize)?
            .to_owned();

        parameters.push(DxbcSignatureParameter {
            semantic_name,
            semantic_index,
            register,
            mask,
            read_write_mask,
        });
    }

    Ok(DxbcSignature { parameters })
}
