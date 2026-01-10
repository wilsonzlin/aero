use aero_dxbc::{DxbcError, DxbcFile, FourCC};

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    // Compute chunk offsets.
    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }

    let total_size = cursor as u32;

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored by parser)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved/unknown
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }

    for (fourcc, data) in chunks {
        bytes.extend_from_slice(&fourcc.0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }

    assert_eq!(bytes.len(), total_size as usize);
    bytes
}

#[test]
fn parse_minimal_dxbc_and_iterate_chunks() {
    let bytes = build_dxbc(&[
        (FourCC(*b"SHDR"), &[1, 2, 3, 4]),
        (FourCC(*b"JUNK"), &[0xaa, 0xbb]),
    ]);

    let file = DxbcFile::parse(&bytes).expect("parse should succeed");
    assert_eq!(file.header().magic, FourCC(*b"DXBC"));
    assert_eq!(file.header().total_size as usize, bytes.len());
    assert_eq!(file.header().chunk_count, 2);

    let chunks: Vec<_> = file.chunks().collect();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].fourcc, FourCC(*b"SHDR"));
    assert_eq!(chunks[0].data, &[1, 2, 3, 4]);
    assert_eq!(chunks[1].fourcc, FourCC(*b"JUNK"));
    assert_eq!(chunks[1].data, &[0xaa, 0xbb]);

    let shdr = file.get_chunk(FourCC(*b"SHDR")).expect("missing SHDR");
    assert_eq!(shdr.data, &[1, 2, 3, 4]);

    let junks: Vec<_> = file.get_chunks(FourCC(*b"JUNK")).collect();
    assert_eq!(junks.len(), 1);
    assert_eq!(junks[0].data, &[0xaa, 0xbb]);

    assert_eq!(file.find_first_shader_chunk().unwrap().fourcc, FourCC(*b"SHDR"));

    let summary = file.debug_summary();
    assert!(summary.contains("SHDR"));
    assert!(summary.contains("JUNK"));
}

#[test]
fn malformed_bad_magic_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    bytes[0..4].copy_from_slice(b"NOPE");

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedHeader { .. }));
}

#[test]
fn malformed_truncated_header_is_error() {
    let bytes = vec![0u8; 10];
    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedHeader { .. }));
}

#[test]
fn malformed_chunk_offset_out_of_bounds_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    // Overwrite the first chunk offset to point outside the container.
    let bad_off = (bytes.len() as u32) + 16;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
}

#[test]
fn malformed_chunk_size_out_of_bounds_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);

    // Locate the chunk header and write an absurd size.
    let header_len = 4 + 16 + 4 + 4 + 4 + 4;
    let chunk_offset = u32::from_le_bytes([
        bytes[header_len - 4],
        bytes[header_len - 3],
        bytes[header_len - 2],
        bytes[header_len - 1],
    ]) as usize;
    bytes[chunk_offset + 4..chunk_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    // Depending on pointer width, this may be detected as integer overflow or bounds.
    assert!(matches!(
        err,
        DxbcError::MalformedOffsets { .. } | DxbcError::OutOfBounds { .. }
    ));
}
