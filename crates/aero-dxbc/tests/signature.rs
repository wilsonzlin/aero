use aero_dxbc::{parse_signature_chunk, DxbcError, DxbcFile, FourCC};

const VS_2_0_SIMPLE_DXBC: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/vs_2_0_simple.dxbc");

fn build_signature_chunk() -> Vec<u8> {
    // Header:
    //   u32 param_count
    //   u32 param_offset (from chunk start)
    //
    // Entry layout (24 bytes):
    //   u32 semantic_name_offset
    //   u32 semantic_index
    //   u32 system_value_type
    //   u32 component_type
    //   u32 register
    //   u8  mask
    //   u8  read_write_mask
    //   u8  stream
    //   u8  min_precision (ignored)
    let mut bytes = Vec::new();

    let param_count = 2u32;
    let param_offset = 8u32;

    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&param_offset.to_le_bytes());

    let table_start = bytes.len();
    assert_eq!(table_start, 8);

    let entry_size = 24usize;
    let string_table_offset = (table_start + (entry_size * param_count as usize)) as u32;

    let pos_name_offset = string_table_offset;
    let tex_name_offset = string_table_offset + ("POSITION\0".len() as u32);

    // POSITION (register 0, xyzw)
    bytes.extend_from_slice(&pos_name_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type (D3D_NAME_UNDEFINED)
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type (float32)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // register
    bytes.extend_from_slice(&u32::from_le_bytes([0xF, 0xF, 0, 0]).to_le_bytes()); // mask/rw/stream/min_prec

    // TEXCOORD0 (register 1, xy)
    bytes.extend_from_slice(&tex_name_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type
    bytes.extend_from_slice(&1u32.to_le_bytes()); // register
    bytes.extend_from_slice(&u32::from_le_bytes([0x3, 0x3, 0, 0]).to_le_bytes()); // mask/rw/stream/min_prec

    bytes.extend_from_slice(b"POSITION\0");
    bytes.extend_from_slice(b"TEXCOORD\0");

    bytes
}

fn build_signature_chunk_v1() -> Vec<u8> {
    // Same payload as `build_signature_chunk`, but uses a 32-byte entry layout
    // where stream/min-precision are stored as DWORDs at the end of each entry.
    let mut bytes = Vec::new();

    let param_count = 2u32;
    let param_offset = 8u32;

    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&param_offset.to_le_bytes());

    let table_start = bytes.len();
    assert_eq!(table_start, 8);

    let entry_size = 32usize;
    let string_table_offset = (table_start + (entry_size * param_count as usize)) as u32;

    let pos_name_offset = string_table_offset;
    let tex_name_offset = string_table_offset + ("POSITION\0".len() as u32);

    // POSITION
    bytes.extend_from_slice(&pos_name_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // register
    bytes.extend_from_slice(&u32::from_le_bytes([0xF, 0xF, 0, 0]).to_le_bytes()); // mask/rw/pad
    bytes.extend_from_slice(&0u32.to_le_bytes()); // stream
    bytes.extend_from_slice(&0u32.to_le_bytes()); // min_precision

    // TEXCOORD0
    bytes.extend_from_slice(&tex_name_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type
    bytes.extend_from_slice(&1u32.to_le_bytes()); // register
    bytes.extend_from_slice(&u32::from_le_bytes([0x3, 0x3, 0, 0]).to_le_bytes()); // mask/rw/pad
    bytes.extend_from_slice(&0u32.to_le_bytes()); // stream
    bytes.extend_from_slice(&0u32.to_le_bytes()); // min_precision

    bytes.extend_from_slice(b"POSITION\0");
    bytes.extend_from_slice(b"TEXCOORD\0");

    bytes
}

fn build_signature_chunk_v1_one_entry(stream: u32) -> Vec<u8> {
    let mut bytes = Vec::new();

    let param_count = 1u32;
    let param_offset = 8u32;

    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&param_offset.to_le_bytes());

    let table_start = bytes.len();
    assert_eq!(table_start, 8);

    let entry_size = 32usize;
    let string_table_offset = (table_start + entry_size) as u32;

    bytes.extend_from_slice(&string_table_offset.to_le_bytes()); // semantic_name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // register
    bytes.extend_from_slice(&u32::from_le_bytes([0xF, 0xF, 0, 0]).to_le_bytes()); // mask/rw/pad
    bytes.extend_from_slice(&stream.to_le_bytes()); // stream
    bytes.extend_from_slice(&0u32.to_le_bytes()); // min_precision

    bytes.extend_from_slice(b"POSITION\0");
    bytes
}

fn build_signature_chunk_v0_one_entry(stream: u8) -> Vec<u8> {
    let mut bytes = Vec::new();

    let param_count = 1u32;
    let param_offset = 8u32;

    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&param_offset.to_le_bytes());

    let table_start = bytes.len();
    assert_eq!(table_start, 8);

    let entry_size = 24usize;
    let string_table_offset = (table_start + entry_size) as u32;

    bytes.extend_from_slice(&string_table_offset.to_le_bytes()); // semantic_name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // register
    bytes.extend_from_slice(&u32::from_le_bytes([0xF, 0xF, stream, 0]).to_le_bytes()); // mask/rw/stream/min_prec

    bytes.extend_from_slice(b"POSITION\0");
    bytes
}

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
fn parse_signature_chunk_two_entries() {
    let bytes = build_signature_chunk();
    let sig = parse_signature_chunk(&bytes).expect("signature parse should succeed");
    assert_eq!(sig.entries.len(), 2);

    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].semantic_index, 0);
    assert_eq!(sig.entries[0].register, 0);
    assert_eq!(sig.entries[0].system_value_type, 0);
    assert_eq!(sig.entries[0].component_type, 3);
    assert_eq!(sig.entries[0].mask, 0xF);
    assert_eq!(sig.entries[0].read_write_mask, 0xF);
    assert_eq!(sig.entries[0].stream, Some(0));

    assert_eq!(sig.entries[1].semantic_name, "TEXCOORD");
    assert_eq!(sig.entries[1].semantic_index, 0);
    assert_eq!(sig.entries[1].register, 1);
    assert_eq!(sig.entries[1].mask, 0x3);
    assert_eq!(sig.entries[1].read_write_mask, 0x3);
}

#[test]
fn parse_signature_chunk_two_entries_v1_layout() {
    let bytes = build_signature_chunk_v1();
    let sig = parse_signature_chunk(&bytes).expect("signature parse should succeed");
    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].mask, 0xF);
    assert_eq!(sig.entries[1].semantic_name, "TEXCOORD");
    assert_eq!(sig.entries[1].mask, 0x3);
}

#[test]
fn parse_signature_chunk_v1_layout_single_entry_stream_is_preserved() {
    let bytes = build_signature_chunk_v1_one_entry(2);
    let sig = parse_signature_chunk(&bytes).expect("signature parse should succeed");
    assert_eq!(sig.entries.len(), 1);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].stream, Some(2));
}

#[test]
fn parse_signature_chunk_v0_layout_single_entry_stream_is_preserved() {
    let bytes = build_signature_chunk_v0_one_entry(2);
    let sig = parse_signature_chunk(&bytes).expect("signature parse should succeed");
    assert_eq!(sig.entries.len(), 1);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].stream, Some(2));
}

#[test]
fn parse_signature_chunk_empty_is_ok() {
    // Some shaders may legitimately have empty signatures (e.g. no patch
    // constants); accept count==0 with any in-bounds offset.
    let bytes = [0u8; 8]; // param_count=0, param_offset=0
    let sig = parse_signature_chunk(&bytes).expect("empty signature parse should succeed");
    assert!(sig.entries.is_empty());
}

#[test]
fn dxbc_get_signature_parses_chunk() {
    let sig_bytes = build_signature_chunk();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISGN"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sig = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_falls_back_to_v1_chunk_id() {
    let sig_bytes = build_signature_chunk();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISG1"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Callers commonly ask for `ISGN`, but some toolchains emit `ISG1`.
    let sig = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_parses_v1_entry_layout() {
    let sig_bytes = build_signature_chunk_v1();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISG1"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Ensure both the `ISG1` chunk lookup and the 32-byte entry layout are handled.
    let sig = dxbc
        .get_signature(FourCC(*b"ISG1"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[1].semantic_name, "TEXCOORD");
}

#[test]
fn dxbc_get_signature_parses_v1_layout_single_entry_stream_is_preserved() {
    let sig_bytes = build_signature_chunk_v1_one_entry(2);
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISG1"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sig = dxbc
        .get_signature(FourCC(*b"ISG1"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 1);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].stream, Some(2));
}

#[test]
fn dxbc_get_signature_uses_fallback_variant_if_primary_is_malformed() {
    let mut bad_bytes = Vec::new();
    bad_bytes.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bad_bytes.extend_from_slice(&4u32.to_le_bytes()); // param_offset into header (invalid)

    let good_bytes = build_signature_chunk();

    let dxbc_bytes = build_dxbc(&[
        (FourCC(*b"ISGN"), &bad_bytes),
        (FourCC(*b"ISG1"), &good_bytes),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Even though ISGN exists, it is malformed. Accept the `ISG1` variant if it
    // parses successfully.
    let sig = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("expected a signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_returns_error_if_only_variant_chunk_is_malformed() {
    let mut bad_bytes = Vec::new();
    bad_bytes.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bad_bytes.extend_from_slice(&4u32.to_le_bytes()); // param_offset into header (invalid)

    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISG1"), &bad_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let err = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("expected a signature chunk")
        .unwrap_err();

    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("ISG1"));
}

#[test]
fn dxbc_get_signature_falls_back_from_v1_to_base_chunk_id() {
    let sig_bytes = build_signature_chunk();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISGN"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Some callers prefer to use the v1 chunk IDs, but the container may still
    // use the base `*SGN` naming.
    let sig = dxbc
        .get_signature(FourCC(*b"ISG1"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn signature_chunk_table_out_of_bounds_is_rejected() {
    // Declares one entry, but doesn't provide enough bytes for the table.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bytes.extend_from_slice(&8u32.to_le_bytes()); // param_offset
    bytes.extend_from_slice(&[0u8; 4]); // truncated

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("signature table"));
}

#[test]
fn signature_chunk_param_offset_into_header_is_rejected() {
    // Declares one entry but points the table offset into the 8-byte header.
    let bytes = [1u32.to_le_bytes(), 4u32.to_le_bytes()].concat();

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("param_offset"));
    assert!(err.context().contains("header"));
}

#[test]
fn signature_chunk_param_offset_unaligned_is_rejected() {
    // Declares one entry but uses a misaligned table offset.
    let bytes = [1u32.to_le_bytes(), 9u32.to_le_bytes()].concat();

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("param_offset"));
    assert!(err.context().contains("aligned"));
}

#[test]
fn signature_chunk_bad_semantic_offset_is_rejected() {
    let mut bytes = build_signature_chunk();
    // Overwrite entry 0 semantic_name_offset to point outside the chunk.
    bytes[8..12].copy_from_slice(&(u32::MAX).to_le_bytes());

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("semantic_name"));
}

#[test]
fn signature_chunk_missing_null_terminator_is_rejected() {
    let mut bytes = build_signature_chunk();
    // Overwrite the last byte (null terminator of the final string) so there's
    // no terminating `\0` in the remaining data.
    *bytes
        .last_mut()
        .expect("signature bytes should be non-empty") = b'X';

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("null terminator"));
}

#[test]
fn signature_chunk_invalid_utf8_is_rejected() {
    let mut bytes = build_signature_chunk();
    let needle = b"POSITION\0";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("expected POSITION string in test chunk");

    // 0xFF is not valid UTF-8.
    bytes[pos] = 0xFF;

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("valid UTF-8"));
}

#[test]
fn signature_chunk_semantic_name_offset_into_table_is_rejected() {
    let mut bytes = build_signature_chunk();
    // Point the first entry's semantic_name_offset at the start of the entry
    // table (offset 8), which should be rejected.
    bytes[8..12].copy_from_slice(&8u32.to_le_bytes());

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("points into signature table"));
}

#[test]
fn signature_chunk_from_real_dxbc_fixture_parses() {
    let dxbc = DxbcFile::parse(VS_2_0_SIMPLE_DXBC).expect("DXBC fixture should parse");

    let isgn = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("fixture should contain ISGN")
        .expect("ISGN should parse");

    assert_eq!(
        isgn.entries
            .iter()
            .map(|e| (
                e.semantic_name.as_str(),
                e.semantic_index,
                e.register,
                e.mask
            ))
            .collect::<Vec<_>>(),
        vec![("POSITION", 0, 0, 0xF), ("TEXCOORD", 0, 1, 0x3)]
    );

    let osgn = dxbc
        .get_signature(FourCC(*b"OSGN"))
        .expect("fixture should contain OSGN")
        .expect("OSGN should parse");

    assert_eq!(
        osgn.entries
            .iter()
            .map(|e| (
                e.semantic_name.as_str(),
                e.semantic_index,
                e.register,
                e.mask
            ))
            .collect::<Vec<_>>(),
        vec![("POSITION", 0, 0, 0xF), ("TEXCOORD", 0, 1, 0x3)]
    );
}
