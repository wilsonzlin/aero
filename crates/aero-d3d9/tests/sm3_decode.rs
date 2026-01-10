use aero_d3d9::sm3::decode::{
    decode_u32_tokens, DclUsage, Opcode, Operand, RegisterFile, SwizzleComponent, TextureType,
};
use aero_d3d9::sm3::types::{ShaderStage, ShaderVersion};

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
fn decode_basic_vs_instructions() {
    // vs_3_0
    let tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        // dcl_position v0
        31u32 | (2u32 << 24) | (0u32 << 16) | (0u32 << 20),
        dst_token(1, 0, 0xF),
        // dcl_texcoord0 v1.xy
        31u32 | (2u32 << 24) | (5u32 << 16) | (0u32 << 20),
        dst_token(1, 1, 0x3),
        // mov r0, v0
        opcode_token(1, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // add r0, r0, c0
        opcode_token(2, 4),
        dst_token(0, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    assert_eq!(
        shader.version,
        ShaderVersion {
            stage: ShaderStage::Vertex,
            major: 3,
            minor: 0
        }
    );

    assert_eq!(shader.instructions[0].opcode, Opcode::Dcl);
    assert_eq!(shader.instructions[0].dcl.as_ref().unwrap().usage, DclUsage::Position);

    let dcl0_reg = match &shader.instructions[0].operands[0] {
        Operand::Dst(dst) => &dst.reg,
        _ => panic!("expected dst operand"),
    };
    assert_eq!(dcl0_reg.file, RegisterFile::Input);
    assert_eq!(dcl0_reg.index, 0);

    assert_eq!(shader.instructions[2].opcode, Opcode::Mov);
    assert_eq!(shader.instructions[3].opcode, Opcode::Add);
    assert_eq!(shader.instructions.last().unwrap().opcode, Opcode::End);
}

#[test]
fn decode_relative_constant_addressing() {
    // mov r0, c1[a0.x]
    let mut c1_rel = src_token(2, 1, 0xE4, 0);
    c1_rel |= 0x0000_2000; // RELATIVE flag

    let tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        opcode_token(1, 4),
        dst_token(0, 0, 0xF),
        c1_rel,
        // a0.x (swizzle = xxxx)
        src_token(3, 0, 0x00, 0),
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let mov = &shader.instructions[0];
    assert_eq!(mov.opcode, Opcode::Mov);

    let src = match &mov.operands[1] {
        Operand::Src(src) => src,
        _ => panic!("expected src operand"),
    };
    assert_eq!(src.reg.file, RegisterFile::Const);
    assert_eq!(src.reg.index, 1);
    let rel = src.reg.relative.as_ref().expect("expected relative addressing");
    assert_eq!(rel.reg.file, RegisterFile::Addr);
    assert_eq!(rel.reg.index, 0);
    assert_eq!(rel.component, SwizzleComponent::X);
}

#[test]
fn decode_predicated_instruction() {
    // add (p0) r0, r0, c0
    let pred_token = src_token(20, 0, 0x00, 0); // p0.x

    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        opcode_token(2, 5) | 0x1000_0000, // predicated
        dst_token(0, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        src_token(2, 0, 0xE4, 0),
        pred_token,
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let add = &shader.instructions[0];
    assert_eq!(add.opcode, Opcode::Add);
    assert!(add.predicate.is_some());
    assert_eq!(add.operands.len(), 3);
    assert_eq!(add.predicate.as_ref().unwrap().reg.file, RegisterFile::Predicate);
}

#[test]
fn decode_sampler_dcl() {
    // dcl_2d s0
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        31u32 | (2u32 << 24) | (2u32 << 16) | (0u32 << 20),
        dst_token(12, 0, 0xF),
        0x0000_FFFF,
    ];

    let shader = decode_u32_tokens(&tokens).unwrap();
    let dcl = &shader.instructions[0];
    assert_eq!(dcl.opcode, Opcode::Dcl);
    assert_eq!(
        dcl.dcl.as_ref().unwrap().usage,
        DclUsage::TextureType(TextureType::Texture2D)
    );
}
