use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, decode_u32_tokens, generate_wgsl, verify_ir};

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

// SM2/3 token streams encode the *total* instruction length in tokens (including the opcode
// token) in bits 24..27, not the operand count.
fn opcode_token(op: u16, operand_tokens: u8) -> u32 {
    // SM2/3 encodes the *total* instruction length (in DWORD tokens), including the opcode token,
    // in bits 24..27. For test readability we accept the operand token count (excluding the opcode
    // token) and add 1 here.
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
fn sm3_dp2_decodes_builds_and_lowers_to_valid_wgsl() {
    // ps_3_0:
    //   def c0, 1.0, 2.0, 0.0, 0.0
    //   def c1, 3.0, 4.0, 0.0, 0.0
    //   setp_eq p0.x, c0.x, c0.x   // p0.x = 1
    //   dp2_sat (p0) r0.xy, c0, c1 // predicated + write mask + result modifier
    //   mov oC0, r0
    //   end
    let dp2_opcode: u16 = 90; // 0x5A

    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 1,2,0,0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        // def c1, 3,4,0,0
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        // setp_eq p0.x, c0.x, c0.x  (cmp code 1 = eq, encoded in opcode token bits 16..)
        opcode_token(78, 3) | (1u32 << 16),
        dst_token(19, 0, 0x1),
        src_token(2, 0, 0x00, 0), // c0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // dp2_sat (p0) r0.xy, c0, c1
        opcode_token(dp2_opcode, 4) | 0x1000_0000 | (1u32 << 20),
        dst_token(0, 0, 0x3),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // predicate p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    // Validate WGSL via naga to ensure WebGPU compatibility.
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(
        wgsl.contains("dot((") && wgsl.contains(").xy"),
        "expected dp2 lowering to use dot(a.xy, b.xy)\n{wgsl}"
    );
    assert!(
        wgsl.contains("if (") && wgsl.contains("p0"),
        "expected predicated dp2 to lower to a WGSL if\n{wgsl}"
    );
    assert!(
        wgsl.contains("clamp("),
        "expected dp2_sat to lower to clamp()\n{wgsl}"
    );
}
