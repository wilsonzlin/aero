use std::fs;

use aero_d3d9::dxbc::robust::{DxbcError, DxbcShader, FourCc, ShaderModel, ShaderType};

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/dxbc/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn parses_vs_2_0_fixture() {
    let bytes = load_fixture("vs_2_0_simple.dxbc");
    let shader = DxbcShader::parse(&bytes).expect("fixture should parse");

    assert_eq!(shader.shader_type, ShaderType::Vertex);
    assert_eq!(shader.shader_model, ShaderModel { major: 2, minor: 0 });
    assert_eq!(shader.key.0, 0x6010_9961_99f0_3b79);

    assert_eq!(shader.unknown_chunks, vec![FourCc::from_str("JUNK")]);

    let refl = shader
        .reflection
        .as_ref()
        .expect("fixture should include RDEF");
    assert_eq!(refl.creator.as_deref(), Some("aero-fixture"));
    assert_eq!(refl.constant_buffers.len(), 1);
    assert_eq!(refl.constant_buffers[0].name, "$Globals");
    assert_eq!(refl.constant_buffers[0].size, 64);
    assert_eq!(refl.constant_buffers[0].variables.len(), 1);
    let var = &refl.constant_buffers[0].variables[0];
    assert_eq!(var.name, "g_mvp");
    assert_eq!(var.offset, 0);
    assert_eq!(var.size, 64);
    assert_eq!(var.ty.rows, 4);
    assert_eq!(var.ty.columns, 4);
    assert!(refl.resources.is_empty());

    let isgn = shader
        .input_signature
        .as_ref()
        .expect("fixture should include ISGN");
    assert_eq!(isgn.parameters.len(), 2);
    assert_eq!(isgn.parameters[0].semantic_name, "POSITION");
    assert_eq!(isgn.parameters[0].semantic_index, 0);
    assert_eq!(isgn.parameters[0].register, 0);
    assert_eq!(isgn.parameters[0].mask, 0xF);

    let osgn = shader
        .output_signature
        .as_ref()
        .expect("fixture should include OSGN");
    assert_eq!(osgn.parameters.len(), 2);
    assert_eq!(osgn.parameters[1].semantic_name, "TEXCOORD");
    assert_eq!(osgn.parameters[1].register, 1);
    assert_eq!(osgn.parameters[1].mask, 0x3);

    assert_eq!(shader.stats.as_deref(), Some(&[11, 0, 0, 0][..]));
}

#[test]
fn parses_ps_2_0_fixture_and_disassembles() {
    let bytes = load_fixture("ps_2_0_sample.dxbc");
    let shader = DxbcShader::parse(&bytes).expect("fixture should parse");

    assert_eq!(shader.shader_type, ShaderType::Pixel);
    assert_eq!(shader.shader_model, ShaderModel { major: 2, minor: 0 });
    assert_eq!(shader.key.0, 0x1f58_3ce3_014c_38e5);

    let refl = shader
        .reflection
        .as_ref()
        .expect("fixture should include RDEF");
    assert_eq!(refl.constant_buffers[0].variables[0].name, "g_color");
    assert_eq!(refl.resources.len(), 2);
    assert_eq!(refl.resources[0].name, "g_texture");
    assert_eq!(refl.resources[0].input_type, 2);
    assert_eq!(refl.resources[0].bind_point, 0);
    assert_eq!(refl.resources[1].name, "g_sampler");
    assert_eq!(refl.resources[1].input_type, 3);
    assert_eq!(refl.resources[1].bind_point, 0);

    let expected_disasm = "\
ps_2_0 ; 15 tokens\n\
0000: version 0xffff0200\n\
0001: dcl t0\n\
0003: texld r0, t0, s0\n\
0007: mul r0, r0, c0\n\
000b: mov oC0, r0\n\
000e: end\n\
";
    assert_eq!(shader.disassemble(), expected_disasm);
}

#[test]
fn parses_vs_3_0_fixture() {
    let bytes = load_fixture("vs_3_0_branch.dxbc");
    let shader = DxbcShader::parse(&bytes).expect("fixture should parse");

    assert_eq!(shader.shader_type, ShaderType::Vertex);
    assert_eq!(shader.shader_model, ShaderModel { major: 3, minor: 0 });
    assert_eq!(shader.key.0, 0x069b_636a_a020_3c78);

    let disasm = shader.disassemble();
    assert!(
        disasm.contains("0006: if b0"),
        "expected disassembly to mention branching\n{disasm}"
    );
}

#[test]
fn parses_ps_3_0_fixture() {
    let bytes = load_fixture("ps_3_0_math.dxbc");
    let shader = DxbcShader::parse(&bytes).expect("fixture should parse");

    assert_eq!(shader.shader_type, ShaderType::Pixel);
    assert_eq!(shader.shader_model, ShaderModel { major: 3, minor: 0 });
    assert_eq!(shader.key.0, 0x88b6_342e_a113_af08);

    let refl = shader
        .reflection
        .as_ref()
        .expect("fixture should include RDEF");
    assert_eq!(refl.constant_buffers[0].variables.len(), 2);
    assert_eq!(refl.constant_buffers[0].variables[0].name, "g_a");
    assert_eq!(refl.constant_buffers[0].variables[1].name, "g_b");
}

#[test]
fn rejects_invalid_magic() {
    match DxbcShader::parse(b"NOPE") {
        Err(DxbcError::InvalidMagic { .. }) => {}
        other => panic!("expected InvalidMagic, got {other:?}"),
    }
}

#[test]
fn rejects_oob_chunk_offset() {
    // Minimal DXBC with chunk_count=1 but offset points past the declared container size.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&36u32.to_le_bytes()); // total size
    bytes.extend_from_slice(&1u32.to_le_bytes()); // chunk count
    bytes.extend_from_slice(&0x1000u32.to_le_bytes()); // chunk offset

    match DxbcShader::parse(&bytes) {
        Err(DxbcError::ChunkOffsetOutOfBounds { .. }) => {}
        other => panic!("expected ChunkOffsetOutOfBounds, got {other:?}"),
    }
}

#[test]
fn rejects_chunk_size_past_end() {
    // DXBC with one chunk whose size runs past the declared container size.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&44u32.to_le_bytes()); // total size
    bytes.extend_from_slice(&1u32.to_le_bytes()); // chunk count
    bytes.extend_from_slice(&36u32.to_le_bytes()); // chunk offset
    bytes.extend_from_slice(b"SHDR");
    bytes.extend_from_slice(&0xffff_ffffu32.to_le_bytes());

    match DxbcShader::parse(&bytes) {
        Err(DxbcError::ChunkDataOutOfBounds { .. }) => {}
        other => panic!("expected ChunkDataOutOfBounds, got {other:?}"),
    }
}
