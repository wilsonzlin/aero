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

    fn parse_signature_with_entry_size(
        fourcc: FourCc,
        bytes: &[u8],
        param_count: u32,
        param_offset: u32,
        entry_size: usize,
    ) -> Result<DxbcSignature, DxbcError> {
        let table_bytes =
            (param_count as usize)
                .checked_mul(entry_size)
                .ok_or(DxbcError::InvalidChunk {
                    fourcc,
                    reason: "parameter count overflow",
                })?;

        let table_start = param_offset as usize;
        if table_start.checked_add(table_bytes).is_none() || table_start + table_bytes > bytes.len()
        {
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

    // Support both common D3D10+ signature entry layouts:
    // - v0: 24-byte entries (`*SGN`, `PCSG`)
    // - v1: 32-byte entries (`*SG1`, `PCG1`)
    //
    // Prefer the layout implied by the FourCC suffix, but accept the other layout as a fallback.
    let prefer_v1 = fourcc.as_bytes()[3] == b'1';
    let primary_size = if prefer_v1 { 32usize } else { 24usize };
    let fallback_size = if prefer_v1 { 24usize } else { 32usize };

    match parse_signature_with_entry_size(fourcc, bytes, param_count, param_offset, primary_size) {
        Ok(sig) => Ok(sig),
        Err(err_primary) => {
            parse_signature_with_entry_size(fourcc, bytes, param_count, param_offset, fallback_size)
                .or(Err(err_primary))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_utils::{
        build_signature_chunk_v0, build_signature_chunk_v1, SignatureEntryDesc,
    };

    #[test]
    fn parses_v0_layout_even_when_fourcc_is_isg1() {
        let bytes = build_signature_chunk_v0(&[SignatureEntryDesc {
            semantic_name: "POSITION",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3,
            register: 0,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        }]);
        let sig = parse_signature(FourCc::from("ISG1"), &bytes).expect("parse signature");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].semantic_name, "POSITION");
    }

    #[test]
    fn parses_v1_layout_even_when_fourcc_is_isgn() {
        let bytes = build_signature_chunk_v1(&[SignatureEntryDesc {
            semantic_name: "POSITION",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3,
            register: 0,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        }]);
        let sig = parse_signature(FourCc::from("ISGN"), &bytes).expect("parse signature");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].semantic_name, "POSITION");
    }
}
