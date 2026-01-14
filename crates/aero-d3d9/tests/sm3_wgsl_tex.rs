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

#[test]
fn wgsl_ps3_texldp_is_valid() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   dcl_2d s0
    //   texldp r0, v0, s0
    //   mov oC0, r0
    //   end
    //
    // texldp is encoded by opcode_token[16] = 1 (see decoder).
    let dcl_texcoord0_v0 = 31u32 | (2u32 << 24) | (5u32 << 16);
    let dcl_2d_s0 = 31u32 | (2u32 << 24) | (2u32 << 16);
    let texldp = opcode_token(0x0042, 3) | (1u32 << 16);
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        dcl_texcoord0_v0,
        dst_token(1, 0, 0xF),
        dcl_2d_s0,
        dst_token(10, 0, 0xF),
        texldp,
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    // Textures/samplers are bound in their own bind group (separate from constants).
    assert!(wgsl.contains("@group(2) @binding(0) var tex0"), "{wgsl}");
    assert!(wgsl.contains("@group(2) @binding(1) var samp0"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_ps3_texkill_discard_is_valid() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   texkill v0
    //   mov oC0, v0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        31u32 | (2u32 << 24) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        opcode_token(65, 1), // texkill
        src_token(1, 0, 0xE4, 0),
        opcode_token(1, 2), // mov
        dst_token(8, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("discard"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}
