use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, decode_u32_tokens, generate_wgsl, verify_ir};

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

fn opcode_token(op: u16, operand_count: u8) -> u32 {
    // D3D9 SM2/SM3 encodes the *total* instruction length in tokens (including the opcode token)
    // in bits 24..27.
    (op as u32) | (((operand_count as u32) + 1) << 24)
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
fn translates_exp_to_wgsl_exp2_with_predication_and_saturate() {
    // ps_3_0
    //
    // def c0, 0.5, -1.0, 2.0, 8.0
    // def c1, 0.0, 0.0, 0.0, 0.0
    // setp_gt p0, c0, c1
    // exp_sat_x2 (p0) r0, c0
    // mov oC0, r0
    // end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 0.5, -1.0, 2.0, 8.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F00_0000,
        0xBF80_0000,
        0x4000_0000,
        0x4100_0000,
        // def c1, 0.0, 0.0, 0.0, 0.0
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // setp_gt p0, c0, c1 (compare code 0 = gt)
        opcode_token(78, 3),
        dst_token(19, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        // exp_sat_x2 (p0.x) r0, c0
        // - predication bit: 0x1000_0000
        // - result modifier bits (20..23): saturate + mul2 shift = 0b0011 => 3 << 20
        opcode_token(14, 3) | 0x1000_0000 | (3u32 << 20),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&shader).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    validate_wgsl(&wgsl.wgsl);

    assert!(wgsl.wgsl.contains("exp2("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("clamp("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("* 2.0"), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("if (p0.x)"), "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn translates_log_to_wgsl_log2_with_saturate_and_shift() {
    // ps_3_0:
    //   def c0, 1.0, 2.0, 4.0, 8.0
    //   log_sat_x2 r0, c0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x4000_0000,
        0x4080_0000,
        0x4100_0000,
        // log_sat_x2 r0, c0 (modbits: saturate + mul2)
        opcode_token(15, 2) | (3u32 << 20),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&shader).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    validate_wgsl(&wgsl.wgsl);

    assert!(wgsl.wgsl.contains("log2("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("clamp("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("* 2.0"), "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn translates_pow_to_wgsl_pow_with_predication_and_modifiers() {
    // ps_3_0:
    //   def c0, 2.0, 4.0, 8.0, 16.0
    //   def c1, 2.0, 0.5, 1.0, 0.0
    //   setp_gt p0, c0, c1
    //   (p0) pow_sat_x2 r0, c0, c1
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x4000_0000,
        0x4080_0000,
        0x4100_0000,
        0x4180_0000,
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x4000_0000,
        0x3F00_0000,
        0x3F80_0000,
        0x0000_0000,
        // setp_gt p0, c0, c1 (compare op 0 = gt)
        opcode_token(78, 3),
        dst_token(19, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        // (p0.x) pow_sat_x2 r0, c0, c1
        opcode_token(32, 4) | 0x1000_0000 | (3u32 << 20),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&shader).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    validate_wgsl(&wgsl.wgsl);

    assert!(wgsl.wgsl.contains("pow("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("clamp("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("* 2.0"), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("if (p0.x)"), "wgsl:\n{}", wgsl.wgsl);
}
#[test]
fn translates_nrm_to_wgsl_normalize() {
    // ps_3_0:
    //   def c0, 1.0, 2.0, 3.0, 4.0
    //   nrm r0, c0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x4000_0000,
        0x4040_0000,
        0x4080_0000,
        opcode_token(36, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&shader).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    validate_wgsl(&wgsl.wgsl);

    assert!(wgsl.wgsl.contains("normalize("), "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn translates_lit_to_wgsl_pow() {
    // ps_3_0:
    //   def c0, 0.5, 0.25, 0.0, 8.0
    //   lit r0, c0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F00_0000,
        0x3E80_0000,
        0x0000_0000,
        0x4100_0000,
        opcode_token(16, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&shader).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    validate_wgsl(&wgsl.wgsl);

    assert!(wgsl.wgsl.contains("pow("), "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn translates_sincos_to_wgsl_sin_cos_with_saturate() {
    // ps_3_0:
    //   def c0, 1.0, 0.0, 0.0, 0.0
    //   def c1, 2.0, 0.0, 0.0, 0.0
    //   def c2, 0.5, 0.0, 0.0, 0.0
    //   sincos_sat r0, c0, c1, c2
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x4000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        opcode_token(81, 5),
        dst_token(2, 2, 0xF),
        0x3F00_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // sincos_sat r0, c0, c1, c2
        opcode_token(37, 4) | (1u32 << 20),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&shader).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    validate_wgsl(&wgsl.wgsl);

    assert!(wgsl.wgsl.contains("sin("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("cos("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("clamp("), "wgsl:\n{}", wgsl.wgsl);
}
