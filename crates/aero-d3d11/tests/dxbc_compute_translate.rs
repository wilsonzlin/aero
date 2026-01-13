use std::fs;

use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, ShaderStage, Sm4Program,
};
use aero_d3d11::sm4::decode_program;

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
fn decodes_and_translates_compute_uav_store_fixture() {
    let bytes = load_fixture("cs_store_uav_raw.dxbc");
    let dxbc = DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse failed");
    assert_eq!(program.stage, ShaderStage::Compute);
    assert_eq!(program.model.major, 5);
    assert_eq!(program.model.minor, 0);

    let module = decode_program(&program).expect("SM5 decode failed");

    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
    let translated =
        translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translation failed");

    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("@workgroup_size(1, 1, 1)"));
}
