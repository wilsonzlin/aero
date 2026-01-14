use std::fs;

use aero_d3d9::{dxbc, sm3};

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/dxbc/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn sm3_fixtures_lower_to_naga_valid_wgsl() {
    for name in [
        "ps_2_0_sample.dxbc",
        "ps_3_0_math.dxbc",
        "vs_3_0_branch.dxbc",
        "vs_2_0_simple.dxbc",
    ] {
        let bytes = load_fixture(name);
        let shdr = dxbc::extract_shader_bytecode(&bytes).expect("extract shader bytecode");
        let decoded = sm3::decode_u8_le_bytes(shdr).expect("sm3 decode");
        let ir = sm3::build_ir(&decoded).expect("sm3 build_ir");
        sm3::verify_ir(&ir).expect("sm3 verify_ir");

        let wgsl = sm3::generate_wgsl(&ir)
            .unwrap_or_else(|e| panic!("wgsl generation failed for {name}: {e}"))
            .wgsl;

        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("wgsl parse failed for {name}: {e}\n{wgsl}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("wgsl validate failed for {name}: {e}\n{wgsl}"));
    }
}
