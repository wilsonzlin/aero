use aero_protocol::aerogpu::aerogpu_ring::{decode_alloc_table_le, AerogpuAllocTableDecodeError};

/// An entry in an AeroGPU allocation table dump (`alloc_id -> GPA` mapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedAllocEntry {
    pub alloc_id: u32,
    pub gpa: u64,
    pub size_bytes: u64,
    pub flags: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum AllocTableDumpError {
    #[error("alloc table decode error: {0:?}")]
    Decode(AerogpuAllocTableDecodeError),
}

/// Decode a raw alloc table dump (little-endian) and return the parsed entries.
pub fn decode_alloc_table_entries_le(
    bytes: &[u8],
) -> Result<Vec<DecodedAllocEntry>, AllocTableDumpError> {
    let table = decode_alloc_table_le(bytes)?;
    Ok(table
        .entries
        .iter()
        .map(|e| DecodedAllocEntry {
            alloc_id: e.alloc_id,
            gpa: e.gpa,
            size_bytes: e.size_bytes,
            flags: e.flags,
        })
        .collect())
}

impl From<AerogpuAllocTableDecodeError> for AllocTableDumpError {
    fn from(value: AerogpuAllocTableDecodeError) -> Self {
        Self::Decode(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
    use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AEROGPU_ALLOC_TABLE_MAGIC};

    fn push_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn build_alloc_table(entries: &[(u32, u64, u64, u32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, AEROGPU_ALLOC_TABLE_MAGIC);
        push_u32(&mut bytes, AEROGPU_ABI_VERSION_U32);
        push_u32(&mut bytes, 0); // size_bytes patched later
        push_u32(&mut bytes, entries.len() as u32);
        push_u32(&mut bytes, AerogpuAllocEntry::SIZE_BYTES as u32);
        push_u32(&mut bytes, 0); // reserved0

        for &(alloc_id, gpa, size_bytes, flags) in entries {
            push_u32(&mut bytes, alloc_id);
            push_u32(&mut bytes, flags);
            push_u64(&mut bytes, gpa);
            push_u64(&mut bytes, size_bytes);
            push_u64(&mut bytes, 0); // reserved0
        }

        let size_bytes = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());
        bytes
    }

    #[test]
    fn decodes_entries_from_valid_table() {
        let table = build_alloc_table(&[
            (10, 0x1122_3344_5566_7788, 0x1000, 1),
            (20, 0x8877_6655_4433_2211, 0x2000, 0),
        ]);

        let entries = decode_alloc_table_entries_le(&table).unwrap();
        assert_eq!(
            entries,
            vec![
                DecodedAllocEntry {
                    alloc_id: 10,
                    gpa: 0x1122_3344_5566_7788,
                    size_bytes: 0x1000,
                    flags: 1
                },
                DecodedAllocEntry {
                    alloc_id: 20,
                    gpa: 0x8877_6655_4433_2211,
                    size_bytes: 0x2000,
                    flags: 0
                }
            ]
        );
    }

    #[test]
    fn rejects_bad_magic_with_clear_error() {
        let mut table = build_alloc_table(&[]);
        table[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        let err = decode_alloc_table_entries_le(&table).unwrap_err();
        assert!(matches!(
            err,
            AllocTableDumpError::Decode(AerogpuAllocTableDecodeError::BadMagic {
                found: 0xDEAD_BEEF
            })
        ));
        assert!(err.to_string().contains("BadMagic"));
    }
}
