use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, decode_u32_tokens, generate_wgsl, verify_ir};

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

fn opcode_token(op: u16, operand_tokens: u8) -> u32 {
    // SM2/3 encodes the *total* instruction length in tokens (including the opcode token) in
    // bits 24..27.
    (op as u32) | (((operand_tokens as u32) + 1) << 24)
}

fn reg_token(regtype: u8, index: u32) -> u32 {
    let low3 = (regtype as u32) & 0x7;
    let high2 = (regtype as u32) & 0x18;
    0x8000_0000 | (low3 << 28) | (high2 << 8) | (index & 0x7FF)
}

fn dst_token(regtype: u8, index: u32, mask: u8) -> u32 {
    reg_token(regtype, index) | ((mask as u32) << 16)
}

fn src_token(regtype: u8, index: u32, swizzle: u8, srcmod: u8) -> u32 {
    reg_token(regtype, index) | ((swizzle as u32) << 16) | ((srcmod as u32) << 24)
}

fn validate_wgsl(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_ps30_writes_odepth_emits_frag_depth() {
    // ps_3_0:
    //   mov oDepth, c0
    //   mov oC0, c0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov oDepth, c0
        opcode_token(1, 2),
        dst_token(9, 0, 0x1), // oDepth.x
        src_token(2, 0, 0xE4, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF), // oC0
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&shader).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    validate_wgsl(&wgsl);

    assert!(wgsl.contains("@builtin(frag_depth)"), "{wgsl}");
    assert!(wgsl.contains("out.depth = oDepth.x;"), "{wgsl}");
}

