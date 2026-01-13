use aero_dxbc::{parse_ctab_chunk, parse_rdef_chunk, DxbcFile, FourCC};

const VS_2_0_SIMPLE_DXBC: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/vs_2_0_simple.dxbc");
const PS_2_0_SAMPLE_DXBC: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/ps_2_0_sample.dxbc");

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
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
fn parse_rdef_resource_bindings_minimal() {
    // Minimal RDEF-like chunk with a single texture bound at t3.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // cb count
    push_u32(&mut chunk, 0); // cb offset
    push_u32(&mut chunk, 1); // resource count
    push_u32(&mut chunk, 28); // resource offset (header size)
    push_u32(&mut chunk, 0); // shader model
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 0); // creator offset

    // Resource entry (32 bytes).
    push_u32(&mut chunk, 60); // name offset
    push_u32(&mut chunk, 0); // type
    push_u32(&mut chunk, 0); // return type
    push_u32(&mut chunk, 0); // dimension
    push_u32(&mut chunk, 0); // num samples
    push_u32(&mut chunk, 3); // bind point
    push_u32(&mut chunk, 1); // bind count
    push_u32(&mut chunk, 0); // flags

    chunk.extend_from_slice(b"tex0\0");

    let rdef = parse_rdef_chunk(&chunk).unwrap();
    assert_eq!(rdef.creator, None);
    assert_eq!(rdef.resources.len(), 1);
    assert_eq!(rdef.resources[0].name, "tex0");
    assert_eq!(rdef.resources[0].bind_point, 3);
    assert_eq!(rdef.resources[0].bind_count, 1);
}

#[test]
fn parse_ctab_constant_table_minimal() {
    // Minimal CTAB chunk with a single constant c0 and target string.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // size (ignored)
    push_u32(&mut chunk, 0); // creator offset
    push_u32(&mut chunk, 0); // version
    push_u32(&mut chunk, 1); // constant count
    push_u32(&mut chunk, 28); // constant info offset
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 48); // target offset (after entry)

    // Constant info entry (20 bytes).
    push_u32(&mut chunk, 55); // name offset (after target string)
    push_u16(&mut chunk, 0); // register set
    push_u16(&mut chunk, 0); // register index
    push_u16(&mut chunk, 1); // register count
    push_u16(&mut chunk, 0); // reserved
    push_u32(&mut chunk, 0); // type info offset
    push_u32(&mut chunk, 0); // default value offset

    chunk.extend_from_slice(b"ps_2_0\0"); // 7 bytes -> next offset 55
    chunk.extend_from_slice(b"C0\0");

    let ctab = parse_ctab_chunk(&chunk).unwrap();
    assert_eq!(ctab.creator, None);
    assert_eq!(ctab.target.as_deref(), Some("ps_2_0"));
    assert_eq!(ctab.constants.len(), 1);
    assert_eq!(ctab.constants[0].name, "C0");
    assert_eq!(ctab.constants[0].register_index, 0);
    assert_eq!(ctab.constants[0].register_count, 1);
}

#[test]
fn dxbc_get_rdef_parses_chunk() {
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // cb count
    push_u32(&mut chunk, 0); // cb offset
    push_u32(&mut chunk, 1); // resource count
    push_u32(&mut chunk, 28); // resource offset (header size)
    push_u32(&mut chunk, 0); // shader model
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 0); // creator offset

    // Resource entry (32 bytes).
    push_u32(&mut chunk, 60); // name offset
    push_u32(&mut chunk, 0); // type
    push_u32(&mut chunk, 0); // return type
    push_u32(&mut chunk, 0); // dimension
    push_u32(&mut chunk, 0); // num samples
    push_u32(&mut chunk, 3); // bind point
    push_u32(&mut chunk, 1); // bind count
    push_u32(&mut chunk, 0); // flags

    chunk.extend_from_slice(b"tex0\0");

    let dxbc_bytes = build_dxbc(&[(FourCC(*b"RDEF"), &chunk)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let rdef = dxbc
        .get_rdef()
        .expect("missing RDEF")
        .expect("RDEF parse should succeed");

    assert_eq!(rdef.resources.len(), 1);
    assert_eq!(rdef.resources[0].name, "tex0");
    assert_eq!(rdef.resources[0].bind_point, 3);
}

#[test]
fn dxbc_get_ctab_parses_chunk() {
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // size (ignored)
    push_u32(&mut chunk, 0); // creator offset
    push_u32(&mut chunk, 0); // version
    push_u32(&mut chunk, 1); // constant count
    push_u32(&mut chunk, 28); // constant info offset
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 48); // target offset (after entry)

    // Constant info entry (20 bytes).
    push_u32(&mut chunk, 55); // name offset (after target string)
    push_u16(&mut chunk, 0); // register set
    push_u16(&mut chunk, 0); // register index
    push_u16(&mut chunk, 1); // register count
    push_u16(&mut chunk, 0); // reserved
    push_u32(&mut chunk, 0); // type info offset
    push_u32(&mut chunk, 0); // default value offset

    chunk.extend_from_slice(b"ps_2_0\0");
    chunk.extend_from_slice(b"C0\0");

    let dxbc_bytes = build_dxbc(&[(FourCC(*b"CTAB"), &chunk)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let ctab = dxbc
        .get_ctab()
        .expect("missing CTAB")
        .expect("CTAB parse should succeed");

    assert_eq!(ctab.target.as_deref(), Some("ps_2_0"));
    assert_eq!(ctab.constants.len(), 1);
    assert_eq!(ctab.constants[0].name, "C0");
}

#[test]
fn dxbc_get_rdef_skips_malformed_duplicate_chunks() {
    let bad_chunk = [0u8; 4]; // truncated RDEF header

    let mut good_chunk = Vec::new();
    push_u32(&mut good_chunk, 0); // cb count
    push_u32(&mut good_chunk, 0); // cb offset
    push_u32(&mut good_chunk, 1); // resource count
    push_u32(&mut good_chunk, 28); // resource offset (header size)
    push_u32(&mut good_chunk, 0); // shader model
    push_u32(&mut good_chunk, 0); // flags
    push_u32(&mut good_chunk, 0); // creator offset

    // Resource entry (32 bytes).
    push_u32(&mut good_chunk, 60); // name offset
    push_u32(&mut good_chunk, 0); // type
    push_u32(&mut good_chunk, 0); // return type
    push_u32(&mut good_chunk, 0); // dimension
    push_u32(&mut good_chunk, 0); // num samples
    push_u32(&mut good_chunk, 3); // bind point
    push_u32(&mut good_chunk, 1); // bind count
    push_u32(&mut good_chunk, 0); // flags
    good_chunk.extend_from_slice(b"tex0\0");

    let dxbc_bytes = build_dxbc(&[
        (FourCC(*b"RDEF"), &bad_chunk),
        (FourCC(*b"RDEF"), &good_chunk),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let rdef = dxbc
        .get_rdef()
        .expect("expected an RDEF chunk")
        .expect("RDEF parse should succeed");
    assert_eq!(rdef.resources.len(), 1);
    assert_eq!(rdef.resources[0].name, "tex0");
}

#[test]
fn dxbc_get_ctab_skips_malformed_duplicate_chunks() {
    let bad_chunk = [0u8; 4]; // truncated CTAB header

    let mut good_chunk = Vec::new();
    push_u32(&mut good_chunk, 0); // size (ignored)
    push_u32(&mut good_chunk, 0); // creator offset
    push_u32(&mut good_chunk, 0); // version
    push_u32(&mut good_chunk, 1); // constant count
    push_u32(&mut good_chunk, 28); // constant info offset
    push_u32(&mut good_chunk, 0); // flags
    push_u32(&mut good_chunk, 48); // target offset (after entry)

    // Constant info entry (20 bytes).
    push_u32(&mut good_chunk, 55); // name offset (after target string)
    push_u16(&mut good_chunk, 0); // register set
    push_u16(&mut good_chunk, 0); // register index
    push_u16(&mut good_chunk, 1); // register count
    push_u16(&mut good_chunk, 0); // reserved
    push_u32(&mut good_chunk, 0); // type info offset
    push_u32(&mut good_chunk, 0); // default value offset
    good_chunk.extend_from_slice(b"ps_2_0\0");
    good_chunk.extend_from_slice(b"C0\0");

    let dxbc_bytes = build_dxbc(&[
        (FourCC(*b"CTAB"), &bad_chunk),
        (FourCC(*b"CTAB"), &good_chunk),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let ctab = dxbc
        .get_ctab()
        .expect("expected a CTAB chunk")
        .expect("CTAB parse should succeed");
    assert_eq!(ctab.target.as_deref(), Some("ps_2_0"));
    assert_eq!(ctab.constants.len(), 1);
    assert_eq!(ctab.constants[0].name, "C0");
}

#[test]
fn rdef_from_real_dxbc_fixture_parses_creator_and_resources() {
    let dxbc = DxbcFile::parse(PS_2_0_SAMPLE_DXBC).expect("DXBC fixture should parse");

    let rdef = dxbc
        .get_rdef()
        .expect("fixture should contain RDEF")
        .expect("RDEF should parse");

    assert_eq!(rdef.creator.as_deref(), Some("aero-fixture"));
    assert_eq!(rdef.resources.len(), 2);

    assert_eq!(rdef.resources[0].name, "g_texture");
    assert_eq!(rdef.resources[0].ty, 2);
    assert_eq!(rdef.resources[0].bind_point, 0);
    assert_eq!(rdef.resources[0].bind_count, 1);

    assert_eq!(rdef.resources[1].name, "g_sampler");
    assert_eq!(rdef.resources[1].ty, 3);
    assert_eq!(rdef.resources[1].bind_point, 0);
    assert_eq!(rdef.resources[1].bind_count, 1);
}

#[test]
fn rdef_from_vertex_shader_fixture_with_no_resources_is_empty() {
    let dxbc = DxbcFile::parse(VS_2_0_SIMPLE_DXBC).expect("DXBC fixture should parse");

    let rdef = dxbc
        .get_rdef()
        .expect("fixture should contain RDEF")
        .expect("RDEF should parse");

    assert_eq!(rdef.creator.as_deref(), Some("aero-fixture"));
    assert!(rdef.resources.is_empty());
}
