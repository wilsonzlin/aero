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

    assert_eq!(
        file.find_first_shader_chunk().unwrap().fourcc,
        FourCC(*b"SHDR")
    );

    let summary = file.debug_summary();
    assert!(summary.contains("SHDR"));
    assert!(summary.contains("JUNK"));
}

#[test]
fn parse_allows_misaligned_chunk_offsets() {
    // Some real-world DXBC containers (and fuzzed inputs) may not maintain strict
    // 4-byte alignment for chunk starts. The parser should handle this safely.
    let bytes = build_dxbc(&[
        (FourCC(*b"SHDR"), &[1]),      // chunk 1 will start at an unaligned offset
        (FourCC(*b"JUNK"), &[2, 3]),   // also make total_size non-4-aligned
    ]);

    // Sanity check: ensure we actually produced a misaligned offset for the
    // second chunk.
    let offset_table_pos = 4 + 16 + 4 + 4 + 4;
    let second_off =
        u32::from_le_bytes(bytes[offset_table_pos + 4..offset_table_pos + 8].try_into().unwrap())
            as usize;
    assert!(!second_off.is_multiple_of(4));

    let file = DxbcFile::parse(&bytes).expect("parse should succeed");
    let chunks: Vec<_> = file.chunks().collect();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].fourcc, FourCC(*b"SHDR"));
    assert_eq!(chunks[0].data, &[1]);
    assert_eq!(chunks[1].fourcc, FourCC(*b"JUNK"));
    assert_eq!(chunks[1].data, &[2, 3]);
}

#[test]
fn malformed_bad_magic_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    bytes[0..4].copy_from_slice(b"NOPE");

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedHeader { .. }));
    assert!(err.context().contains("bad magic"));
}

#[test]
fn malformed_truncated_header_is_error() {
    let bytes = vec![0u8; 10];
    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedHeader { .. }));
    assert!(err.context().contains("need at least"));
    assert!(err.context().contains("got"));
}

#[test]
fn malformed_total_size_smaller_than_header_is_error() {
    let mut bytes = build_dxbc(&[]);
    // total_size field is at offset 24.
    bytes[24..28].copy_from_slice(&(0u32).to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedHeader { .. }));
    assert!(err.context().contains("total_size"));
    assert!(err.context().contains("smaller than header"));
}

#[test]
fn malformed_total_size_exceeds_buffer_len_is_error() {
    let mut bytes = build_dxbc(&[]);
    // total_size field is at offset 24.
    let bad_total_size = (bytes.len() as u32) + 1;
    bytes[24..28].copy_from_slice(&bad_total_size.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("total_size"));
    assert!(err.context().contains("exceeds buffer length"));
}

#[test]
fn malformed_total_size_truncates_chunk_payload_is_error() {
    // Keep the buffer length unchanged but shrink the declared total_size so it
    // truncates the final byte of the chunk payload. This ensures the parser
    // uses declared `total_size` as the authoritative bounds.
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3, 4])]);
    let bad_total_size = (bytes.len() as u32) - 1;
    bytes[24..28].copy_from_slice(&bad_total_size.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("SHDR"));
    assert!(err.context().contains("outside total_size"));
}

#[test]
fn malformed_total_size_truncates_chunk_header_is_error() {
    // Shrink total_size to end exactly at the end of the chunk offset table,
    // leaving no room for the chunk header itself.
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3, 4])]);
    let offset_table_end = 4 + 16 + 4 + 4 + 4 + 4;
    bytes[24..28].copy_from_slice(&(offset_table_end as u32).to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("header"));
    assert!(err.context().contains("outside total_size"));
}

#[test]
fn malformed_truncated_chunk_offset_table_is_error() {
    // DXBC header declaring one chunk, but missing the chunk offset table entry.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved
    bytes.extend_from_slice(&32u32.to_le_bytes()); // total_size
    bytes.extend_from_slice(&1u32.to_le_bytes()); // chunk_count
    assert_eq!(bytes.len(), 32);

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk offset table"));
}

#[test]
fn malformed_chunk_count_makes_offset_table_oob_is_error() {
    // Declare a huge chunk_count but keep total_size minimal, ensuring the offset
    // table end computation stays safe and is rejected.
    let mut bytes = build_dxbc(&[]);
    // Use the maximum accepted chunk count so we still exercise offset table
    // bounds checks (values above this are rejected earlier).
    bytes[28..32].copy_from_slice(&(4096u32).to_le_bytes()); // chunk_count

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk offset table") || err.context().contains("chunk_count"));
}

#[test]
fn malformed_last_chunk_offset_is_reported_with_large_chunk_count() {
    // Use the maximum allowed chunk_count to ensure offset-table indexing stays safe
    // up to the last entry. All chunk offsets are valid except the last one.
    let chunk_count = 4096u32;
    let offset_table_end = 4 + 16 + 4 + 4 + 4 + (chunk_count as usize * 4);
    let total_size = offset_table_end + 8; // one minimal chunk header after the table

    let mut bytes = Vec::with_capacity(total_size);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved
    bytes.extend_from_slice(&(total_size as u32).to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());

    for i in 0..chunk_count {
        let off = if i == chunk_count - 1 {
            0u32
        } else {
            offset_table_end as u32
        };
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    assert_eq!(bytes.len(), offset_table_end);

    // Minimal chunk header at the end of the offset table.
    bytes.extend_from_slice(b"JUNK");
    bytes.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(bytes.len(), total_size);

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk 4095"));
    assert!(err.context().contains("points into DXBC header"));
}

#[test]
fn malformed_last_chunk_offset_out_of_bounds_with_large_chunk_count() {
    // Similar to `malformed_last_chunk_offset_is_reported_with_large_chunk_count`, but
    // make the final chunk offset point *past* the end of the container to ensure the
    // OutOfBounds path reports the correct chunk index.
    let chunk_count = 4096u32;
    let offset_table_end = 4 + 16 + 4 + 4 + 4 + (chunk_count as usize * 4);
    let total_size = offset_table_end + 8; // one minimal chunk header after the table

    let mut bytes = Vec::with_capacity(total_size);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved
    bytes.extend_from_slice(&(total_size as u32).to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());

    for i in 0..chunk_count {
        let off = if i == chunk_count - 1 {
            total_size as u32
        } else {
            offset_table_end as u32
        };
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    assert_eq!(bytes.len(), offset_table_end);

    // Minimal chunk header at the end of the offset table.
    bytes.extend_from_slice(b"JUNK");
    bytes.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(bytes.len(), total_size);

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 4095"));
    assert!(err.context().contains("header"));
    assert!(err.context().contains("outside total_size"));
}

#[test]
fn malformed_chunk_offset_points_into_header_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    // Overwrite the first chunk offset to point into the DXBC header.
    let bad_off = 0u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("points into DXBC header"));
}

#[test]
fn malformed_chunk_offset_points_into_header_tail_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    // Point at the final byte of the fixed header (still inside header).
    let bad_off = 31u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("points into DXBC header"));
}

#[test]
fn malformed_chunk_offset_points_into_offset_table_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    // Point the first chunk offset into the 4-byte chunk offset table itself.
    // Use a misaligned offset to ensure we never assume 4-byte alignment.
    let bad_off = 33u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("points into chunk offset table"));
}

#[test]
fn malformed_chunk_offset_points_to_offset_table_start_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    // Point exactly at the start of the chunk offset table (aligned case).
    let bad_off = 32u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("points into chunk offset table"));
}

#[test]
fn malformed_second_chunk_offset_is_error_and_mentions_index() {
    let mut bytes = build_dxbc(&[
        (FourCC(*b"SHDR"), &[1, 2, 3, 4]),
        (FourCC(*b"JUNK"), &[0xaa]),
    ]);
    // Overwrite the second chunk offset to point into the DXBC header.
    let bad_off = 0u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    let second_offset_pos = offset_table_pos + 4;
    bytes[second_offset_pos..second_offset_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    // Ensure the error context refers to chunk index 1 (the second entry).
    assert!(err.context().contains("chunk 1"));
    assert!(err.context().contains("points into DXBC header"));
}

#[test]
fn malformed_second_chunk_offset_points_into_offset_table_mentions_index() {
    let mut bytes = build_dxbc(&[
        (FourCC(*b"SHDR"), &[1, 2, 3, 4]),
        (FourCC(*b"JUNK"), &[0xaa]),
    ]);
    // For two chunks, the chunk offset table spans 32..40. Point the second
    // chunk offset into the middle of that table.
    let bad_off = 36u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    let second_offset_pos = offset_table_pos + 4;
    bytes[second_offset_pos..second_offset_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }));
    assert!(err.context().contains("chunk 1"));
    assert!(err.context().contains("points into chunk offset table"));
}

#[test]
fn malformed_second_chunk_offset_out_of_bounds_mentions_index() {
    let mut bytes = build_dxbc(&[
        (FourCC(*b"SHDR"), &[1, 2, 3, 4]),
        (FourCC(*b"JUNK"), &[0xaa]),
    ]);
    // Overwrite the second chunk offset to point at the end of the container,
    // leaving no room for the chunk header.
    let bad_off = bytes.len() as u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    let second_offset_pos = offset_table_pos + 4;
    bytes[second_offset_pos..second_offset_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 1"));
    assert!(err.context().contains("header"));
}

#[test]
fn malformed_second_chunk_size_out_of_bounds_mentions_index() {
    let mut bytes = build_dxbc(&[
        (FourCC(*b"SHDR"), &[1, 2, 3, 4]),
        (FourCC(*b"JUNK"), &[0xaa, 0xbb]),
    ]);

    // Locate the second chunk header and write an absurd size.
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    let second_chunk_offset = u32::from_le_bytes(
        bytes[offset_table_pos + 4..offset_table_pos + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    bytes[second_chunk_offset + 4..second_chunk_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    // Depending on pointer width, this may be detected as integer overflow or bounds.
    assert!(matches!(
        err,
        DxbcError::MalformedOffsets { .. } | DxbcError::OutOfBounds { .. }
    ));
    assert!(err.context().contains("chunk 1"));
    assert!(err.context().contains("size") || err.context().contains("data"));
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
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("header"));
}

#[test]
fn malformed_chunk_offset_equal_to_total_size_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    // Point the chunk offset exactly at the end of the container, which leaves
    // no room for the 8-byte chunk header.
    let bad_off = bytes.len() as u32;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("header"));
}

#[test]
fn malformed_chunk_offset_truncates_chunk_header_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3, 4])]);
    // Point the chunk offset into the final 4 bytes of the file so that reading
    // the 8-byte chunk header would run past the end.
    let bad_off = (bytes.len() as u32) - 4;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("header"));
    assert!(err.context().contains("outside total_size"));
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
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("overflows") || err.context().contains("outside total_size"));
}

#[test]
fn rejects_excessive_chunk_count() {
    // DXBC header with an absurd chunk_count. The parser should reject this without attempting to
    // validate an enormous chunk-offset table.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved
    bytes.extend_from_slice(&32u32.to_le_bytes()); // total size
    bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // chunk_count

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::MalformedOffsets { .. }), "{err:?}");
    assert!(
        err.context().contains("exceeds maximum"),
        "unexpected error context: {}",
        err.context()
    );
}

#[test]
fn malformed_chunk_size_extends_past_file_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3, 4])]);

    // Find the chunk header and set the size so the payload would extend 1 byte
    // past the end of the declared container.
    let offset_table_pos = 4 + 16 + 4 + 4 + 4;
    let chunk_offset =
        u32::from_le_bytes(bytes[offset_table_pos..offset_table_pos + 4].try_into().unwrap())
            as usize;
    let data_start = chunk_offset + 8;
    let bad_size = (bytes.len() - data_start + 1) as u32;
    bytes[chunk_offset + 4..chunk_offset + 8].copy_from_slice(&bad_size.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("SHDR"));
    assert!(err.context().contains("data"));
    assert!(err.context().contains("outside total_size"));
}

#[test]
fn malformed_chunk_size_nonzero_with_no_payload_is_error() {
    // Container where the chunk header is at the end of the file (size=0, no payload),
    // then we lie and declare size=1.
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[])]);

    let offset_table_pos = 4 + 16 + 4 + 4 + 4;
    let chunk_offset =
        u32::from_le_bytes(bytes[offset_table_pos..offset_table_pos + 4].try_into().unwrap())
            as usize;
    bytes[chunk_offset + 4..chunk_offset + 8].copy_from_slice(&1u32.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("data"));
    assert!(err.context().contains("outside total_size"));
}

#[test]
fn malformed_chunk_offset_integer_wrap_is_error() {
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3])]);
    // Set the chunk offset to a value that will overflow `offset + 8` on 32-bit
    // platforms. On 64-bit platforms it is simply outside the container.
    let bad_off = u32::MAX;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(
        err,
        DxbcError::MalformedOffsets { .. } | DxbcError::OutOfBounds { .. }
    ));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("overflows") || err.context().contains("outside total_size"));
}

#[test]
fn malformed_chunk_offset_misaligned_after_offset_table_is_error() {
    // Chunk offsets are not guaranteed to be 4-byte aligned in the wild; ensure
    // that a misaligned offset is handled safely (no panic / OOB read).
    let mut bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[1, 2, 3, 4])]);

    // For a single-chunk DXBC, the chunk offset table ends at 36. Use a
    // deliberately misaligned offset just after it.
    let offset_table_end = 4 + 16 + 4 + 4 + 4 + 4;
    let bad_off = (offset_table_end as u32) + 1;
    let offset_table_pos = 4 + 16 + 4 + 4 + 4; // start of chunk offsets
    bytes[offset_table_pos..offset_table_pos + 4].copy_from_slice(&bad_off.to_le_bytes());

    let err = DxbcFile::parse(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::OutOfBounds { .. }));
    assert!(err.context().contains("chunk 0"));
    assert!(err.context().contains("data"));
}
