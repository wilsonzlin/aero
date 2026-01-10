use aero_d3d9::sm3::{build_ir, decode_u32_tokens, verify_ir};
use aero_d3d9::sm3::types::ShaderStage;

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

fn opcode_token(op: u16, length: u8) -> u32 {
    (op as u32) | ((length as u32) << 24)
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
fn ir_snapshot_ps3_tex_ifc() {
    // ps_3_0:
    //   dcl_texcoord0 v0.xy
    //   dcl_2d s0
    //   def c0, 0.5, 0.0, 0.0, 0.0
    //   texld r0, v0, s0
    //   ifc_gt r0.x, c0.x
    //     mov oC0, r0
    //   else
    //     mov oC0, c0
    //   endif
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0.xy
        31u32 | (2u32 << 24) | (5u32 << 16) | (0u32 << 20),
        dst_token(1, 0, 0x3),
        // dcl_2d s0
        31u32 | (2u32 << 24) | (2u32 << 16) | (0u32 << 20),
        dst_token(12, 0, 0xF),
        // def c0, 0.5, 0, 0, 0
        opcode_token(65, 6),
        dst_token(2, 0, 0xF),
        0x3F00_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // texld r0, v0, s0
        opcode_token(51, 4),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(12, 0, 0xE4, 0),
        // ifc_gt r0.x, c0.x  (compare op 0 = gt)
        opcode_token(41, 3),
        src_token(0, 0, 0x00, 0),
        src_token(2, 0, 0x00, 0),
        // mov oC0, r0
        opcode_token(1, 3),
        dst_token(10, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // else
        opcode_token(42, 1),
        // mov oC0, c0
        opcode_token(1, 3),
        dst_token(10, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // endif
        opcode_token(43, 1),
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    insta::assert_snapshot!(ir.to_string());
}

