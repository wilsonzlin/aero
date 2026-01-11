use std::fs;

use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, translate_sm4_to_wgsl, DxbcFile, FourCC,
    RegFile, ShaderStage, Sm4Inst, Sm4Program, SrcKind,
};

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

fn assert_wgsl_parses(wgsl: &str) {
    naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
}

#[test]
fn parses_and_translates_sm4_vs_passthrough_fixture() {
    let bytes = load_fixture("vs_passthrough.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    assert!(dxbc.get_chunk(FOURCC_ISGN).is_some(), "missing ISGN chunk");
    assert!(dxbc.get_chunk(FOURCC_OSGN).is_some(), "missing OSGN chunk");
    assert!(dxbc.get_chunk(FOURCC_SHDR).is_some(), "missing SHDR chunk");

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    assert!(signatures.isgn.is_some(), "missing parsed ISGN");
    assert!(signatures.osgn.is_some(), "missing parsed OSGN");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse failed");
    assert_eq!(program.stage, ShaderStage::Vertex);
    assert_eq!(program.model.major, 4);

    // Ensure the token stream is decodable by the real SM4 decoder (not just the bootstrap MOV/RET
    // parser in `wgsl.rs`).
    let module = program.decode().expect("SM4 decode failed");
    assert_eq!(module.instructions.len(), 3);
    assert!(matches!(
        &module.instructions[0],
        Sm4Inst::Mov { dst, src }
            if dst.reg.file == RegFile::Output && dst.reg.index == 0
                && matches!(src.kind, SrcKind::Register(r) if r.file == RegFile::Input && r.index == 0)
    ));

    let wgsl_full = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .expect("signature-driven translation failed")
        .wgsl;
    assert_wgsl_parses(&wgsl_full);

    let wgsl = translate_sm4_to_wgsl(&program).expect("translation failed").wgsl;
    assert_wgsl_parses(&wgsl);
    assert!(wgsl.contains("@vertex"));
    assert!(wgsl.contains("out.pos = input.v0"));
    assert!(wgsl.contains("out.o1 = input.v1"));
}

#[test]
fn parses_and_translates_sm4_ps_passthrough_fixture() {
    let bytes = load_fixture("ps_passthrough.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    assert!(dxbc.get_chunk(FOURCC_ISGN).is_some(), "missing ISGN chunk");
    assert!(dxbc.get_chunk(FOURCC_OSGN).is_some(), "missing OSGN chunk");
    assert!(dxbc.get_chunk(FOURCC_SHDR).is_some(), "missing SHDR chunk");

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    assert!(signatures.isgn.is_some(), "missing parsed ISGN");
    assert!(signatures.osgn.is_some(), "missing parsed OSGN");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse failed");
    assert_eq!(program.stage, ShaderStage::Pixel);
    assert_eq!(program.model.major, 4);

    let module = program.decode().expect("SM4 decode failed");
    assert_eq!(module.instructions.len(), 2);
    assert!(matches!(
        &module.instructions[0],
        Sm4Inst::Mov { dst, src }
            if dst.reg.file == RegFile::Output && dst.reg.index == 0
                && matches!(src.kind, SrcKind::Register(r) if r.file == RegFile::Input && r.index == 1)
    ));

    let wgsl_full = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .expect("signature-driven translation failed")
        .wgsl;
    assert_wgsl_parses(&wgsl_full);

    let wgsl = translate_sm4_to_wgsl(&program).expect("translation failed").wgsl;
    assert_wgsl_parses(&wgsl);
    assert!(wgsl.contains("@fragment"));
    assert!(wgsl.contains("return input.v1"));
}
