use crate::FourCC;

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Description of a signature entry when building synthetic DXBC signature chunks in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureEntryDesc<'a> {
    /// Semantic name (e.g. `"POSITION"` or `"SV_Target"`).
    pub semantic_name: &'a str,
    /// Semantic index (e.g. `0` for `TEXCOORD0`).
    pub semantic_index: u32,
    /// System-value type (`D3D_NAME_*`).
    pub system_value_type: u32,
    /// Component type (`D3D_REGISTER_COMPONENT_*`).
    pub component_type: u32,
    /// Register index assigned by the compiler.
    pub register: u32,
    /// Component mask (`D3D_COMPONENT_MASK`).
    pub mask: u8,
    /// Read/write mask.
    pub read_write_mask: u8,
    /// Stream index (typically 0; used by geometry shader stream-output).
    pub stream: u32,
}

const SIGNATURE_HEADER_LEN: usize = 8;
const SIGNATURE_ENTRY_LEN_V0: usize = 24;
const SIGNATURE_ENTRY_LEN_V1: usize = 32;

/// Builds a `*SGN`-style (v0) DXBC signature chunk payload (24-byte entries).
///
/// This is the classic entry layout used by `ISGN`/`OSGN`/`PSGN`.
pub fn build_signature_chunk_v0(entries: &[SignatureEntryDesc<'_>]) -> Vec<u8> {
    build_signature_chunk_with_entry_size(entries, SIGNATURE_ENTRY_LEN_V0)
}

/// Builds a `*SG1`-style (v1) DXBC signature chunk payload (32-byte entries).
///
/// This is the extended entry layout used by `ISG1`/`OSG1`/`PSG1`.
pub fn build_signature_chunk_v1(entries: &[SignatureEntryDesc<'_>]) -> Vec<u8> {
    build_signature_chunk_with_entry_size(entries, SIGNATURE_ENTRY_LEN_V1)
}

/// Builds a signature chunk payload matching the entry layout normally used by the given `fourcc`.
///
/// - `*SGN` → v0 (24-byte entries)
/// - `*SG1` → v1 (32-byte entries)
pub fn build_signature_chunk_for_fourcc(
    fourcc: FourCC,
    entries: &[SignatureEntryDesc<'_>],
) -> Vec<u8> {
    if fourcc.0[3] == b'1' {
        build_signature_chunk_v1(entries)
    } else {
        build_signature_chunk_v0(entries)
    }
}

fn build_signature_chunk_with_entry_size(
    entries: &[SignatureEntryDesc<'_>],
    entry_size: usize,
) -> Vec<u8> {
    assert!(
        entry_size == SIGNATURE_ENTRY_LEN_V0 || entry_size == SIGNATURE_ENTRY_LEN_V1,
        "unsupported signature entry size {entry_size}"
    );

    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes()); // param_count
    out.extend_from_slice(&(SIGNATURE_HEADER_LEN as u32).to_le_bytes()); // param_offset

    let table_start = out.len();
    out.resize(table_start + entries.len() * entry_size, 0);

    for (i, e) in entries.iter().enumerate() {
        let semantic_name_offset = out.len() as u32;
        out.extend_from_slice(e.semantic_name.as_bytes());
        out.push(0);
        // Pad strings to 4-byte alignment to match common toolchain output and existing tests.
        out.resize(align4(out.len()), 0);

        let base = table_start + i * entry_size;
        out[base..base + 4].copy_from_slice(&semantic_name_offset.to_le_bytes());
        out[base + 4..base + 8].copy_from_slice(&e.semantic_index.to_le_bytes());
        out[base + 8..base + 12].copy_from_slice(&e.system_value_type.to_le_bytes());
        out[base + 12..base + 16].copy_from_slice(&e.component_type.to_le_bytes());
        out[base + 16..base + 20].copy_from_slice(&e.register.to_le_bytes());

        match entry_size {
            SIGNATURE_ENTRY_LEN_V0 => {
                out[base + 20] = e.mask;
                out[base + 21] = e.read_write_mask;
                out[base + 22] = u8::try_from(e.stream)
                    .unwrap_or_else(|_| panic!("signature stream {} does not fit in u8", e.stream));
                out[base + 23] = 0; // min_precision (unused)
            }
            SIGNATURE_ENTRY_LEN_V1 => {
                out[base + 20] = e.mask;
                out[base + 21] = e.read_write_mask;
                // bytes 22..23 are reserved/unused (keep as 0)
                out[base + 24..base + 28].copy_from_slice(&e.stream.to_le_bytes());
                out[base + 28..base + 32].copy_from_slice(&0u32.to_le_bytes()); // min_precision
            }
            _ => unreachable!(),
        }
    }

    out
}

/// Builds a minimal `DXBC` container containing the provided chunks.
///
/// The resulting blob has:
/// - a valid `DXBC` header (`DXBC` magic + checksum + reserved + `total_size` + chunk count),
/// - a correct chunk offset table,
/// - and a correct `total_size`.
///
/// The checksum field is **not** computed; it is set to all zeros. This is
/// intentional: `aero-dxbc` does not require checksum correctness during
/// parsing, and most tests only need a structurally-valid container.
pub fn build_container(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    // Header layout:
    // - magic:      4 bytes ("DXBC")
    // - checksum:  16 bytes (MD5; unused here)
    // - reserved:   4 bytes (usually 1)
    // - total_size: 4 bytes
    // - chunk_count:4 bytes
    // - chunk_offsets: chunk_count * 4 bytes
    // - chunks:
    //     - fourcc: 4 bytes
    //     - size:   4 bytes
    //     - data:   size bytes
    let header_size = 4 + 16 + 4 + 4 + 4 + (4 * chunks.len());
    let chunk_bytes = chunks
        .iter()
        .map(|(_, data)| align4(8 + data.len()))
        .sum::<usize>();

    let mut out = Vec::with_capacity(header_size + chunk_bytes);

    out.extend_from_slice(b"DXBC");
    out.extend_from_slice(&[0u8; 16]); // checksum
    out.extend_from_slice(&1u32.to_le_bytes()); // reserved
    out.extend_from_slice(&0u32.to_le_bytes()); // total_size placeholder

    let chunk_count = u32::try_from(chunks.len()).expect("DXBC chunk_count does not fit in u32");
    out.extend_from_slice(&chunk_count.to_le_bytes());

    // Reserve space for the chunk offset table and fill it in once we know the offsets.
    let offsets_pos = out.len();
    out.resize(out.len() + 4 * chunks.len(), 0);

    let mut offsets = Vec::with_capacity(chunks.len());
    for (fourcc, data) in chunks {
        let offset = u32::try_from(out.len()).expect("DXBC chunk offset does not fit in u32");
        offsets.push(offset);

        let chunk_size = u32::try_from(data.len()).expect("DXBC chunk size does not fit in u32");
        out.extend_from_slice(&fourcc.0);
        out.extend_from_slice(&chunk_size.to_le_bytes());
        out.extend_from_slice(data);
        out.resize(align4(out.len()), 0);
    }

    // Fill offsets.
    for (i, offset) in offsets.iter().enumerate() {
        let pos = offsets_pos + i * 4;
        out[pos..pos + 4].copy_from_slice(&offset.to_le_bytes());
    }

    // Fill total_size.
    let total_size = u32::try_from(out.len()).expect("DXBC total_size does not fit in u32");
    let total_size_pos = 4 + 16 + 4;
    out[total_size_pos..total_size_pos + 4].copy_from_slice(&total_size.to_le_bytes());

    out
}

/// Convenience wrapper for [`build_container`] that accepts chunk payloads as owned `Vec<u8>`.
///
/// This is handy for tests that build chunk data inline (e.g. `vec![0u8; 32]`)
/// and want to pass it directly in the chunk list.
pub fn build_container_owned(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(FourCC, &[u8])> = chunks
        .iter()
        .map(|(fourcc, data)| (*fourcc, data.as_slice()))
        .collect();
    build_container(&refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DxbcFile;

    #[test]
    fn build_container_roundtrips_through_parser() {
        let shdr = [1u8, 2, 3, 4];
        let bytes = build_container(&[(FourCC(*b"SHDR"), &shdr)]);

        let file = DxbcFile::parse(&bytes).expect("built container should parse");
        assert_eq!(file.header().magic, FourCC(*b"DXBC"));
        assert_eq!(file.header().total_size as usize, bytes.len());
        assert_eq!(file.header().chunk_count, 1);

        let chunk = file.get_chunk(FourCC(*b"SHDR")).expect("missing SHDR");
        assert_eq!(chunk.data, &shdr);
    }
}
