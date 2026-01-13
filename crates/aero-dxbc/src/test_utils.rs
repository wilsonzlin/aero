use crate::FourCC;

fn align4(n: usize) -> usize {
    (n + 3) & !3
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
