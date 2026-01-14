use crate::{
    parse_signature_chunk, parse_signature_chunk_with_fourcc, test_utils as dxbc_test_utils,
    DxbcError, DxbcFile, FourCC,
};

const VS_2_0_SIMPLE_DXBC: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/vs_2_0_simple.dxbc");

fn build_signature_chunk() -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v0(&[
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "POSITION",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: 0,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        },
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "TEXCOORD",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: 1,
            mask: 0x3,
            read_write_mask: 0x3,
            stream: 0,
            min_precision: 0,
        },
    ])
}

fn build_signature_chunk_with_registers(pos_reg: u32, tex_reg: u32) -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v0(&[
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "POSITION",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: pos_reg,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        },
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "TEXCOORD",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: tex_reg,
            mask: 0x3,
            read_write_mask: 0x3,
            stream: 0,
            min_precision: 0,
        },
    ])
}

fn build_signature_chunk_v1_with_registers(pos_reg: u32, tex_reg: u32) -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v1(&[
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "POSITION",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: pos_reg,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        },
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "TEXCOORD",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: tex_reg,
            mask: 0x3,
            read_write_mask: 0x3,
            stream: 0,
            min_precision: 0,
        },
    ])
}

fn build_signature_chunk_v1() -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v1(&[
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "POSITION",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: 0,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        },
        dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "TEXCOORD",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: 1,
            mask: 0x3,
            read_write_mask: 0x3,
            stream: 0,
            min_precision: 0,
        },
    ])
}

fn build_signature_chunk_v1_one_entry(stream: u32) -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v1(&[dxbc_test_utils::SignatureEntryDesc {
        semantic_name: "POSITION",
        semantic_index: 0,
        system_value_type: 0,
        component_type: 3, // float32
        register: 0,
        mask: 0xF,
        read_write_mask: 0xF,
        stream,
        min_precision: 0,
    }])
}

fn build_signature_chunk_v0_one_entry(stream: u8) -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v0(&[dxbc_test_utils::SignatureEntryDesc {
        semantic_name: "POSITION",
        semantic_index: 0,
        system_value_type: 0,
        component_type: 3, // float32
        register: 0,
        mask: 0xF,
        read_write_mask: 0xF,
        stream: u32::from(stream),
        min_precision: 0,
    }])
}

fn build_signature_chunk_v0_one_entry_padded(stream: u8) -> Vec<u8> {
    // A v0 (24-byte) entry layout with extra padding between the entry table
    // and string table. This exercises that `DxbcFile::get_signature` prefers
    // the v0 layout for `*SGN` chunk IDs even if the v1 heuristic could match.
    dxbc_test_utils::build_signature_chunk_v0_with_table_padding(
        &[dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "POSITION",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3, // float32
            register: 0,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: u32::from(stream),
            min_precision: 0,
        }],
        8,
    )
}

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    dxbc_test_utils::build_container(chunks)
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
    assert_eq!(sig.entries[0].read_write_mask, 0xF);
    assert_eq!(sig.entries[0].system_value_type, 0);
    assert_eq!(sig.entries[0].component_type, 3);
    assert_eq!(sig.entries[1].semantic_name, "TEXCOORD");
    assert_eq!(sig.entries[1].mask, 0x3);
    assert_eq!(sig.entries[1].read_write_mask, 0x3);
    assert_eq!(sig.entries[1].system_value_type, 0);
    assert_eq!(sig.entries[1].component_type, 3);
}

#[test]
fn parse_signature_chunk_with_fourcc_prefers_v1_layout() {
    let bytes = build_signature_chunk_v1_one_entry(2);
    let sig = parse_signature_chunk_with_fourcc(FourCC(*b"ISG1"), &bytes)
        .expect("signature parse should succeed");
    assert_eq!(sig.entries.len(), 1);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].stream, Some(2));
}

#[test]
fn parse_signature_chunk_with_fourcc_falls_back_to_v0_layout_when_entry_table_is_padded() {
    // Some toolchains emit v0 (24-byte) signature entry tables under a `*G1` chunk ID, even with
    // extra padding between the entry table and string table. Ensure the parser doesn't
    // misinterpret the padding bytes as the v1 `stream`/`min_precision` DWORDs.
    let bytes = build_signature_chunk_v0_one_entry_padded(2);
    let sig = parse_signature_chunk_with_fourcc(FourCC(*b"ISG1"), &bytes)
        .expect("signature parse should succeed");
    assert_eq!(sig.entries.len(), 1);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].stream, Some(2));
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
fn parse_signature_chunk_detects_padded_v0_layout() {
    // This chunk doesn't carry a FourCC, so `parse_signature_chunk` must infer
    // the entry layout from the payload. Ensure that a padded v0 layout isn't
    // mis-detected as the 32-byte v1 layout.
    let bytes = build_signature_chunk_v0_one_entry_padded(2);
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
fn parse_signature_chunk_empty_with_oob_offset_is_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u32.to_le_bytes()); // param_count
    bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // param_offset

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("param_offset"));
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
fn dxbc_get_signature_missing_chunk_returns_none() {
    let dxbc_bytes = build_dxbc(&[]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");
    assert!(dxbc.get_signature(FourCC(*b"ISGN")).is_none());
}

#[test]
fn dxbc_get_signature_prefers_exact_fourcc_over_variant() {
    let isgn_bytes = build_signature_chunk_with_registers(0, 1);
    let isg1_bytes = build_signature_chunk_v1_with_registers(10, 11);

    let dxbc_bytes = build_dxbc(&[
        (FourCC(*b"ISGN"), &isgn_bytes),
        (FourCC(*b"ISG1"), &isg1_bytes),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let isgn = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");
    assert_eq!(isgn.entries[0].register, 0);

    let isg1 = dxbc
        .get_signature(FourCC(*b"ISG1"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");
    assert_eq!(isg1.entries[0].register, 10);
}

#[test]
fn dxbc_get_signature_skips_malformed_duplicate_chunks() {
    let mut bad_bytes = Vec::new();
    bad_bytes.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bad_bytes.extend_from_slice(&4u32.to_le_bytes()); // param_offset into header (invalid)

    let good_bytes = build_signature_chunk();

    let dxbc_bytes = build_dxbc(&[
        (FourCC(*b"ISGN"), &bad_bytes),
        (FourCC(*b"ISGN"), &good_bytes),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sig = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("expected a signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_parses_psgn_chunk() {
    let sig_bytes = build_signature_chunk();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PSGN"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sig = dxbc
        .get_signature(FourCC(*b"PSGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_falls_back_to_psg1_chunk() {
    let sig_bytes = build_signature_chunk_v1();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PSG1"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Callers commonly ask for `PSGN`, but some toolchains emit `PSG1`.
    let sig = dxbc
        .get_signature(FourCC(*b"PSGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_parses_pcsg_chunk() {
    let sig_bytes = build_signature_chunk();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PCSG"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sig = dxbc
        .get_signature(FourCC(*b"PCSG"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_falls_back_to_pcg1_chunk() {
    let sig_bytes = build_signature_chunk_v1();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PCG1"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Callers commonly ask for `PCSG`, but some toolchains emit `PCG1`.
    let sig = dxbc
        .get_signature(FourCC(*b"PCSG"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_falls_back_to_pcsg_chunk() {
    let sig_bytes = build_signature_chunk();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PCSG"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Conversely, callers may request the `*G1` variant directly; accept `PCSG` in that case.
    let sig = dxbc
        .get_signature(FourCC(*b"PCG1"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_parses_osgn_chunk() {
    let sig_bytes = build_signature_chunk();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"OSGN"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sig = dxbc
        .get_signature(FourCC(*b"OSGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_falls_back_to_osg1_chunk() {
    let sig_bytes = build_signature_chunk_v1();
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"OSG1"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    // Callers commonly ask for `OSGN`, but some toolchains emit `OSG1`.
    let sig = dxbc
        .get_signature(FourCC(*b"OSGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 2);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
}

#[test]
fn dxbc_get_signature_prefers_v0_layout_for_sgn_chunk_ids() {
    let sig_bytes = build_signature_chunk_v0_one_entry_padded(2);
    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISGN"), &sig_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sig = dxbc
        .get_signature(FourCC(*b"ISGN"))
        .expect("missing signature chunk")
        .expect("signature parse should succeed");

    assert_eq!(sig.entries.len(), 1);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].stream, Some(2));
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
    assert_eq!(sig.entries[0].read_write_mask, 0xF);
    assert_eq!(sig.entries[1].semantic_name, "TEXCOORD");
    assert_eq!(sig.entries[1].read_write_mask, 0x3);
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
fn signature_chunk_semantic_name_offset_into_header_is_rejected() {
    let mut bytes = build_signature_chunk();
    // Overwrite entry 0 semantic_name_offset to point into the 8-byte header.
    bytes[8..12].copy_from_slice(&4u32.to_le_bytes());

    let err = parse_signature_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("points into signature header"));
}

#[test]
fn signature_chunk_missing_null_terminator_is_rejected() {
    // Build a well-formed chunk with a semantic name that doesn't require string-table padding
    // ("ABC\0" is already 4-byte aligned). This ensures the null terminator is at the end of the
    // payload, so overwriting the last byte removes it entirely.
    let mut bytes =
        dxbc_test_utils::build_signature_chunk_v0(&[dxbc_test_utils::SignatureEntryDesc {
            semantic_name: "ABC",
            semantic_index: 0,
            system_value_type: 0,
            component_type: 3,
            register: 0,
            mask: 0xF,
            read_write_mask: 0xF,
            stream: 0,
            min_precision: 0,
        }]);
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
