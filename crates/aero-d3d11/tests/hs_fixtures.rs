use std::fs;

use aero_d3d11::sm4::decode_program;
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, FourCC, ShaderStage, Sm4Program,
};

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_PCSG: FourCC = FourCC(*b"PCSG");
const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

fn assert_wgsl_validates(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("generated WGSL failed to validate");
}

#[test]
fn parses_and_translates_sm5_hs_fixture() {
    let bytes = load_fixture("hs_minimal.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    assert!(dxbc.get_chunk(FOURCC_ISGN).is_some(), "missing ISGN chunk");
    assert!(dxbc.get_chunk(FOURCC_OSGN).is_some(), "missing OSGN chunk");
    assert!(dxbc.get_chunk(FOURCC_PCSG).is_some(), "missing PCSG chunk");
    assert!(dxbc.get_chunk(FOURCC_SHEX).is_some(), "missing SHEX chunk");

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    assert!(signatures.isgn.is_some(), "missing parsed ISGN");
    assert!(signatures.osgn.is_some(), "missing parsed OSGN");
    assert!(
        signatures.pcsg.is_some() || signatures.psgn.is_some(),
        "missing parsed PCSG/PSGN"
    );

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse failed");
    assert_eq!(program.stage, ShaderStage::Hull);

    let module = decode_program(&program).expect("SM5 decode failed");

    let translated =
        translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("HS translate failed");

    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("@compute") && translated.wgsl.contains("fn hs_main"),
        "expected HS translation to generate a compute entry point:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("struct HsRegBuffer { data: array<vec4<u32>> };"),
        "expected HS stage interface register files to be typeless vec4<u32>:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("hs_in: HsRegBuffer;"),
        "expected HS translation to declare hs_in as HsRegBuffer:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("hs_out_cp: HsRegBuffer;"),
        "expected HS translation to declare hs_out_cp as HsRegBuffer:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("hs_patch_constants_buf: HsRegBuffer;"),
        "expected HS translation to declare hs_patch_constants_buf as HsRegBuffer:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("hs_tess_factors: HsRegBuffer;"),
        "expected HS translation to declare hs_tess_factors as HsRegBuffer:\n{}",
        translated.wgsl
    );

    assert!(
        translated
            .reflection
            .outputs
            .iter()
            .any(|p| p.semantic_name.eq_ignore_ascii_case("SV_TessFactor")),
        "expected reflection outputs to include SV_TessFactor, got: {:#?}",
        translated.reflection.outputs
    );
    assert!(
        translated
            .reflection
            .outputs
            .iter()
            .any(|p| p.semantic_name.eq_ignore_ascii_case("SV_InsideTessFactor")),
        "expected reflection outputs to include SV_InsideTessFactor, got: {:#?}",
        translated.reflection.outputs
    );
}
