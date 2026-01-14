use std::fs;

use aero_d3d11::sm4::decode_program;
use aero_d3d11::{parse_signatures, translate_sm4_to_wgsl, DxbcFile, FourCC, ShaderStage, Sm4Program};

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_PSGN: FourCC = FourCC(*b"PSGN");
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
fn parses_and_translates_sm5_ds_tri_integer_fixture() {
    let bytes = load_fixture("ds_tri_integer.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    assert!(dxbc.get_chunk(FOURCC_SHEX).is_some(), "missing SHEX chunk");
    assert!(dxbc.get_chunk(FOURCC_ISGN).is_some(), "missing ISGN chunk");
    assert!(dxbc.get_chunk(FOURCC_OSGN).is_some(), "missing OSGN chunk");
    assert!(dxbc.get_chunk(FOURCC_PSGN).is_some(), "missing PSGN chunk");

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    assert!(signatures.isgn.is_some(), "missing parsed ISGN");
    assert!(signatures.osgn.is_some(), "missing parsed OSGN");
    assert!(signatures.psgn.is_some(), "missing parsed PSGN");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4/5 parse failed");
    assert_eq!(program.stage, ShaderStage::Domain);
    assert_eq!(program.model.major, 5);

    let module = decode_program(&program).expect("SM4/5 decode failed");
    assert_eq!(module.stage, ShaderStage::Domain);

    let translated = translate_sm4_to_wgsl(&dxbc, &module, &signatures)
        .expect("signature-driven translation failed");
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("fn ds_main"));
    assert!(
        translated.wgsl.contains("ds_in_cp"),
        "expected DS WGSL to include HS control-point buffer plumbing:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("ds_in_pc"),
        "expected DS WGSL to include HS patch-constant buffer plumbing:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("ds_domain_location"),
        "expected DS WGSL to include SV_DomainLocation plumbing:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("ds_primitive_id"),
        "expected DS WGSL to include SV_PrimitiveID plumbing:\n{}",
        translated.wgsl
    );

    assert_wgsl_validates(&translated.wgsl);
}
