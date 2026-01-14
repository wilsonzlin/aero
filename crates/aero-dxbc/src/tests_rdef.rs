use crate::{parse_rdef_chunk, DxbcError, DxbcFile, FourCC};

const VS_2_0_SIMPLE_DXBC: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/vs_2_0_simple.dxbc");
const PS_2_0_SAMPLE_DXBC: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/ps_2_0_sample.dxbc");

const FOURCC_RDEF: FourCC = FourCC(*b"RDEF");

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    let b = &bytes[offset..offset + 4];
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

#[test]
fn rdef_chunk_from_real_vs_fixture_parses() {
    let dxbc = DxbcFile::parse(VS_2_0_SIMPLE_DXBC).expect("DXBC fixture should parse");
    let rdef_payload = dxbc.get_chunk(FOURCC_RDEF).expect("missing RDEF").data;

    let rdef = parse_rdef_chunk(rdef_payload).expect("RDEF should parse");
    assert_eq!(rdef.creator.as_deref(), Some("aero-fixture"));

    assert_eq!(rdef.constant_buffers.len(), 1);
    let cb = &rdef.constant_buffers[0];
    assert_eq!(cb.name, "$Globals");
    assert_eq!(cb.size, 64);
    assert_eq!(cb.bind_point, None);
    assert!(cb.bind_count.is_none());

    assert_eq!(cb.variables.len(), 1);
    let var = &cb.variables[0];
    assert_eq!(var.name, "g_mvp");
    assert_eq!(var.offset, 0);
    assert_eq!(var.size, 64);
    assert_eq!(var.ty.rows, 4);
    assert_eq!(var.ty.columns, 4);

    assert!(rdef.bound_resources.is_empty());
}

#[test]
fn rdef_chunk_from_real_ps_fixture_parses_resources() {
    let dxbc = DxbcFile::parse(PS_2_0_SAMPLE_DXBC).expect("DXBC fixture should parse");
    let rdef_payload = dxbc.get_chunk(FOURCC_RDEF).expect("missing RDEF").data;

    let rdef = parse_rdef_chunk(rdef_payload).expect("RDEF should parse");
    assert!(!rdef.constant_buffers.is_empty());
    assert_eq!(rdef.constant_buffers[0].variables[0].name, "g_color");

    assert_eq!(rdef.bound_resources.len(), 2);
    assert_eq!(rdef.bound_resources[0].name, "g_texture");
    assert_eq!(rdef.bound_resources[0].input_type, 2); // D3D_SIT_TEXTURE
    assert_eq!(rdef.bound_resources[0].bind_point, 0);

    assert_eq!(rdef.bound_resources[1].name, "g_sampler");
    assert_eq!(rdef.bound_resources[1].input_type, 3); // D3D_SIT_SAMPLER
    assert_eq!(rdef.bound_resources[1].bind_point, 0);
}

#[test]
fn rdef_chunk_truncated_header_is_rejected() {
    let bytes = vec![0u8; 10];
    let err = parse_rdef_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(err.context().contains("header"));
}

#[test]
fn rdef_chunk_truncated_constant_buffer_table_is_rejected() {
    let dxbc = DxbcFile::parse(VS_2_0_SIMPLE_DXBC).expect("DXBC fixture should parse");
    let mut bytes = dxbc
        .get_chunk(FOURCC_RDEF)
        .expect("missing RDEF")
        .data
        .to_vec();

    // Avoid tripping over the creator string pointer when truncating.
    bytes[24..28].copy_from_slice(&0u32.to_le_bytes());
    // And avoid the resource binding table pointer.
    bytes[8..12].copy_from_slice(&0u32.to_le_bytes()); // rb_count
    bytes[12..16].copy_from_slice(&0u32.to_le_bytes()); // rb_offset

    let cb_count = read_u32_le(&bytes, 0) as usize;
    let cb_offset = read_u32_le(&bytes, 4) as usize;
    let needed = cb_offset + cb_count * 24;
    bytes.truncate(needed.saturating_sub(1));

    let err = parse_rdef_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(
        err.context().contains("constant buffer table"),
        "unexpected error context: {}",
        err.context()
    );
}

#[test]
fn rdef_chunk_bad_variable_table_offset_is_rejected() {
    let dxbc = DxbcFile::parse(VS_2_0_SIMPLE_DXBC).expect("DXBC fixture should parse");
    let mut bytes = dxbc
        .get_chunk(FOURCC_RDEF)
        .expect("missing RDEF")
        .data
        .to_vec();

    // First cbuffer desc is at cb_offset; patch its var_offset field to be absurd.
    let cb_offset = read_u32_le(&bytes, 4) as usize;
    let var_offset_field = cb_offset + 8;
    bytes[var_offset_field..var_offset_field + 4].copy_from_slice(&u32::MAX.to_le_bytes());

    let err = parse_rdef_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(
        err.context().contains("variable table"),
        "unexpected error context: {}",
        err.context()
    );
}

#[test]
fn rdef_chunk_invalid_string_pointer_is_rejected() {
    let dxbc = DxbcFile::parse(VS_2_0_SIMPLE_DXBC).expect("DXBC fixture should parse");
    let mut bytes = dxbc
        .get_chunk(FOURCC_RDEF)
        .expect("missing RDEF")
        .data
        .to_vec();

    // Patch the first cbuffer name_offset to point at the final byte, then ensure that byte is
    // non-zero so there is no null terminator.
    let cb_offset = read_u32_le(&bytes, 4) as usize;
    let name_offset_field = cb_offset;
    let bad_off = (bytes.len().saturating_sub(1)) as u32;
    bytes[name_offset_field..name_offset_field + 4].copy_from_slice(&bad_off.to_le_bytes());
    if let Some(last) = bytes.last_mut() {
        *last = b'X';
    }

    let err = parse_rdef_chunk(&bytes).unwrap_err();
    assert!(matches!(err, DxbcError::InvalidChunk { .. }));
    assert!(
        err.context().contains("name"),
        "unexpected error context: {}",
        err.context()
    );
}
