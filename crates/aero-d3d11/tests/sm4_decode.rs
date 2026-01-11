use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{OperandModifier, RegFile, RegisterRef, Sm4Inst, Sm4Module, Sm4Program, SrcKind, SrcOperand, Swizzle, TextureRef, WriteMask};

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout:
    // type in bits 16.., major in bits 4..7, minor in bits 0..3.
    let version = ((stage_type as u32) << 16) | (5u32 << 4) | 0u32;
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn opcode_token_with_sat(opcode: u32, len_without_ext: u32) -> Vec<u32> {
    // Extended opcode token (type 0) with saturate bit set at bit 13.
    let opcode_token = opcode | ((len_without_ext + 1) << OPCODE_LEN_SHIFT) | OPCODE_EXTENDED_BIT;
    let ext = 0u32 | (1u32 << 13);
    vec![opcode_token, ext]
}

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
    extended: bool,
) -> u32 {
    let mut token = 0u32;
    token |= num_components & OPERAND_NUM_COMPONENTS_MASK;
    token |= (selection_mode & OPERAND_SELECTION_MODE_MASK) << OPERAND_SELECTION_MODE_SHIFT;
    token |= (ty & OPERAND_TYPE_MASK) << OPERAND_TYPE_SHIFT;
    token |= (component_sel & OPERAND_COMPONENT_SELECTION_MASK) << OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & OPERAND_INDEX_DIMENSION_MASK) << OPERAND_INDEX_DIMENSION_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX0_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX1_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX2_REP_SHIFT;
    if extended {
        token |= OPERAND_EXTENDED_BIT;
    }
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32)
        | ((swz[1] as u32) << 2)
        | ((swz[2] as u32) << 4)
        | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1, false),
        idx,
    ]
}

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle, modifier: OperandModifier) -> Vec<u32> {
    let needs_ext = !matches!(modifier, OperandModifier::None);
    let num_components = match ty {
        OPERAND_TYPE_SAMPLER | OPERAND_TYPE_RESOURCE => 0,
        _ => 2,
    };
    let selection_mode = if num_components == 0 {
        OPERAND_SEL_MASK
    } else {
        OPERAND_SEL_SWIZZLE
    };
    let token = operand_token(
        ty,
        num_components,
        selection_mode,
        swizzle_bits(swizzle.0),
        indices.len() as u32,
        needs_ext,
    );
    let mut out = Vec::new();
    out.push(token);
    if needs_ext {
        let mod_bits = match modifier {
            OperandModifier::None => 0,
            OperandModifier::Neg => 1,
            OperandModifier::Abs => 2,
            OperandModifier::AbsNeg => 3,
        };
        out.push((mod_bits << 6) | 0u32);
    }
    out.extend_from_slice(indices);
    out
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(Swizzle::XYZW.0),
        0,
        false,
    ));
    out.extend_from_slice(&values);
    out
}

fn imm32_scalar(value: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_IMMEDIATE32, 1, OPERAND_SEL_SELECT1, 0, 0, false),
        value,
    ]
}

fn dst(file: RegFile, index: u32, mask: WriteMask) -> aero_d3d11::DstOperand {
    aero_d3d11::DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_reg(file: RegFile, index: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef { file, index }),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

fn src_cb(slot: u32, reg: u32, swizzle: Swizzle) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ConstantBuffer { slot, reg },
        swizzle,
        modifier: OperandModifier::None,
    }
}

fn src_imm(bits: [u32; 4]) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32(bits),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

#[test]
fn decodes_arithmetic_and_skips_decls() {
    const DCL_DUMMY: u32 = 0x100;

    let mut body = Vec::<u32>::new();

    // A couple of declaration-like tokens that should be skipped.
    body.extend_from_slice(&[
        opcode_token(DCL_DUMMY, 3),
        operand_token(OPERAND_TYPE_INPUT, 2, OPERAND_SEL_MASK, 0xF, 1, false),
        0,
    ]);
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 1, 2), 4]);

    // mov r0, v0
    let mut mov = vec![opcode_token(OPCODE_MOV, 5)];
    mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov.extend_from_slice(&reg_src(
        OPERAND_TYPE_INPUT,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&mov);

    // add_sat r1, -abs(r0), l(0.5, 1.0, 2.0, 3.0)
    let imm = imm32_vec4([
        0.5f32.to_bits(),
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
    ]);
    let src0 = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::AbsNeg,
    );
    let dst0 = reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW);
    let len_without_ext = 1u32 + dst0.len() as u32 + src0.len() as u32 + imm.len() as u32;
    let mut add = opcode_token_with_sat(OPCODE_ADD, len_without_ext);
    add.extend_from_slice(&dst0);
    add.extend_from_slice(&src0);
    add.extend_from_slice(&imm);
    body.extend_from_slice(&add);

    // mul r1, r1, cb0[0].wzyx
    let mut mul = vec![opcode_token(OPCODE_MUL, 1 + 2 + 2 + 3)];
    mul.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    mul.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    mul.extend_from_slice(&reg_src(
        OPERAND_TYPE_CONSTANT_BUFFER,
        &[0, 0],
        Swizzle([3, 2, 1, 0]),
        OperandModifier::None,
    ));
    body.extend_from_slice(&mul);

    // mad r1, r1, r0, r0
    let mut mad = vec![opcode_token(OPCODE_MAD, 1 + 2 + 2 + 2 + 2)];
    mad.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    mad.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    mad.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    mad.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&mad);

    // dp3 r2.x, r1, r0
    let mut dp3 = vec![opcode_token(OPCODE_DP3, 1 + 2 + 2 + 2)];
    dp3.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::X));
    dp3.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    dp3.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&dp3);

    // dp4 r2.x, r1, r0
    let mut dp4 = vec![opcode_token(OPCODE_DP4, 1 + 2 + 2 + 2)];
    dp4.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::X));
    dp4.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    dp4.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&dp4);

    // min r3, r1, r0
    let mut min = vec![opcode_token(OPCODE_MIN, 1 + 2 + 2 + 2)];
    min.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    min.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    min.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&min);

    // max r3, r1, r0
    let mut max = vec![opcode_token(OPCODE_MAX, 1 + 2 + 2 + 2)];
    max.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    max.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    max.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&max);

    // rcp r4, r3
    let mut rcp = vec![opcode_token(OPCODE_RCP, 1 + 2 + 2)];
    rcp.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    rcp.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[3],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&rcp);

    // rsq r4, r3
    let mut rsq = vec![opcode_token(OPCODE_RSQ, 1 + 2 + 2)];
    rsq.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    rsq.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[3],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&rsq);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = program.decode().expect("decode");

    let f = |v: f32| v.to_bits();
    let mut add_dst = dst(RegFile::Temp, 1, WriteMask::XYZW);
    add_dst.saturate = true;
    assert_eq!(
        module,
        Sm4Module {
            stage: aero_d3d11::ShaderStage::Pixel,
            instructions: vec![
                Sm4Inst::Mov {
                    dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                    src: src_reg(RegFile::Input, 0),
                },
                Sm4Inst::Add {
                    dst: add_dst,
                    a: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 0
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::AbsNeg,
                    },
                    b: src_imm([f(0.5), f(1.0), f(2.0), f(3.0)]),
                },
                Sm4Inst::Mul {
                    dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                    a: src_reg(RegFile::Temp, 1),
                    b: src_cb(0, 0, Swizzle([3, 2, 1, 0])),
                },
                Sm4Inst::Mad {
                    dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                    a: src_reg(RegFile::Temp, 1),
                    b: src_reg(RegFile::Temp, 0),
                    c: src_reg(RegFile::Temp, 0),
                },
                Sm4Inst::Dp3 {
                    dst: dst(RegFile::Temp, 2, WriteMask::X),
                    a: src_reg(RegFile::Temp, 1),
                    b: src_reg(RegFile::Temp, 0),
                },
                Sm4Inst::Dp4 {
                    dst: dst(RegFile::Temp, 2, WriteMask::X),
                    a: src_reg(RegFile::Temp, 1),
                    b: src_reg(RegFile::Temp, 0),
                },
                Sm4Inst::Min {
                    dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
                    a: src_reg(RegFile::Temp, 1),
                    b: src_reg(RegFile::Temp, 0),
                },
                Sm4Inst::Max {
                    dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
                    a: src_reg(RegFile::Temp, 1),
                    b: src_reg(RegFile::Temp, 0),
                },
                Sm4Inst::Rcp {
                    dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
                    src: src_reg(RegFile::Temp, 3),
                },
                Sm4Inst::Rsq {
                    dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
                    src: src_reg(RegFile::Temp, 3),
                },
                Sm4Inst::Ret,
            ],
        }
    );
}

#[test]
fn decodes_sample_and_sample_l() {
    const DCL_DUMMY: u32 = 0x200;
    let mut body = Vec::<u32>::new();

    // Decls to skip.
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 2), 1]);

    // sample r0, v0, t0, s0
    let mut sample = vec![opcode_token(OPCODE_SAMPLE, 1 + 2 + 2 + 2 + 2)];
    sample.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    sample.extend_from_slice(&reg_src(
        OPERAND_TYPE_INPUT,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    sample.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    sample.extend_from_slice(&reg_src(
        OPERAND_TYPE_SAMPLER,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&sample);

    // sample_l r1, v0, t0, s0, l(0)
    let lod = imm32_scalar(0f32.to_bits());
    let mut sample_l = vec![opcode_token(
        OPCODE_SAMPLE_L,
        (1 + 2 + 2 + 2 + 2 + lod.len()) as u32,
    )];
    sample_l.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    sample_l.extend_from_slice(&reg_src(
        OPERAND_TYPE_INPUT,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    sample_l.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    sample_l.extend_from_slice(&reg_src(
        OPERAND_TYPE_SAMPLER,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    sample_l.extend_from_slice(&lod);
    body.extend_from_slice(&sample_l);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = program.decode().expect("decode");

    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Sample { .. })));
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::SampleL { .. })));

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Sample {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            coord: src_reg(RegFile::Input, 0),
            texture: TextureRef { slot: 0 },
            sampler: aero_d3d11::SamplerRef { slot: 0 },
        }
    );
}

