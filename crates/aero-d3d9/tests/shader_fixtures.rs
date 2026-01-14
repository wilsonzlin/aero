use aero_d3d9::{dxbc, shader, sm3};

fn validate_wgsl(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/dxbc/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn legacy_shader_parser_accepts_fxc_fixtures() {
    // Ensure the legacy SM2/SM3 parser can handle real `D3DCompiler` output (DXBC wrappers).
    //
    // Note: This does not validate correctness of the shader output, only that the end-to-end
    // translate-to-WGSL pipeline works without panicking or rejecting the token format.
    for name in [
        "ps_2_0_sample.dxbc",
        "ps_3_0_math.dxbc",
        "vs_2_0_simple.dxbc",
        "vs_3_0_branch.dxbc",
    ] {
        let bytes = load_fixture(name);
        let program = shader::parse(&bytes).unwrap();
        let ir = shader::to_ir(&program);
        let wgsl = shader::generate_wgsl(&ir).unwrap();
        validate_wgsl(&wgsl.wgsl);
    }
}

#[test]
fn sm3_parser_accepts_fxc_fixtures() {
    // Mirror `legacy_shader_parser_accepts_fxc_fixtures`, but exercise the new SM3 pipeline.
    for name in [
        "vs_2_0_simple.dxbc",
        "vs_3_0_branch.dxbc",
    ] {
        let bytes = load_fixture(name);
        let shdr = dxbc::extract_shader_bytecode(&bytes).unwrap();
        let decoded = sm3::decode_u8_le_bytes(shdr).unwrap();
        let ir = sm3::build_ir(&decoded).unwrap();
        sm3::verify_ir(&ir).unwrap();
        let wgsl = sm3::generate_wgsl(&ir).unwrap();
        validate_wgsl(&wgsl.wgsl);
    }
}
