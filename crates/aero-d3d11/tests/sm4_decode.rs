use aero_d3d11::sm4::decode::Sm4DecodeErrorKind;
use aero_d3d11::sm4::{decode_program, opcode::*};
use aero_d3d11::{
    BufferKind, BufferRef, CmpOp, CmpType, HsDomain, HsOutputTopology, HsPartitioning,
    HullShaderPhase, OperandModifier, RegFile, RegisterRef, ShaderModel, Sm4Decl, Sm4Inst,
    Sm4Module, Sm4Program, SrcKind, SrcOperand, Swizzle, TextureRef, UavRef, WriteMask,
};

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout:
    // type in bits 16.., major in bits 4..7, minor in bits 0..3.
    let version = ((stage_type as u32) << 16) | (5u32 << 4);
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
    let ext = 1u32 << 13;
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
    token |=
        (component_sel & OPERAND_COMPONENT_SELECTION_MASK) << OPERAND_COMPONENT_SELECTION_SHIFT;
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
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1, false),
        idx,
    ]
}

fn uav_operand(slot: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(
            OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
            0,
            OPERAND_SEL_MASK,
            mask.0 as u32,
            1,
            false,
        ),
        slot,
    ]
}

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle, modifier: OperandModifier) -> Vec<u32> {
    let needs_ext = !matches!(modifier, OperandModifier::None);
    let num_components = match ty {
        OPERAND_TYPE_SAMPLER | OPERAND_TYPE_RESOURCE | OPERAND_TYPE_UNORDERED_ACCESS_VIEW => 0,
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
        let mod_bits: u32 = match modifier {
            OperandModifier::None => 0,
            OperandModifier::Neg => 1,
            OperandModifier::Abs => 2,
            OperandModifier::AbsNeg => 3,
        };
        out.push(mod_bits << 6);
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
        operand_token(
            OPERAND_TYPE_IMMEDIATE32,
            1,
            OPERAND_SEL_SELECT1,
            0,
            0,
            false,
        ),
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
fn decodes_geometry_shader_input_with_vertex_index() {
    // mov r0, v0[1]
    let mut body = Vec::<u32>::new();

    let mut mov = vec![opcode_token(OPCODE_MOV, 1 + 2 + 1 + 2)];
    mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov.extend_from_slice(&reg_src(
        OPERAND_TYPE_INPUT,
        &[0, 1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&mov);
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 2 is geometry shader.
    let tokens = make_sm5_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Mov {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            src: SrcOperand {
                kind: SrcKind::GsInput { reg: 0, vertex: 1 },
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            }
        }
    );
}

#[test]
fn rejects_geometry_shader_input_with_non_immediate_vertex_index_representation() {
    // Create `mov r0, v0[?]` where the vertex index uses a non-immediate index representation.
    //
    // Our decoder only supports immediate indices for 2D-indexed GS inputs, and must surface a
    // precise `UnsupportedIndexRepresentation` error.
    let mut body = Vec::<u32>::new();

    // mov r0, v0[0] (but with invalid index1 representation)
    let mut mov = vec![opcode_token(OPCODE_MOV, 1 + 2 + 1 + 1)];
    mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));

    let mut src_tok = operand_token(
        OPERAND_TYPE_INPUT,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(Swizzle::XYZW.0),
        2,
        false,
    );
    // Force index1 representation to a non-immediate encoding (value 1).
    src_tok |= 1 << OPERAND_INDEX1_REP_SHIFT;
    mov.push(src_tok);
    // Provide only the first index (register index). The decode should fail before attempting to
    // read the second index.
    mov.push(0);

    body.extend_from_slice(&mov);
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 2 is geometry shader.
    let tokens = make_sm5_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let err = decode_program(&program).expect_err("expected decode to fail");

    assert!(
        matches!(
            err.kind,
            Sm4DecodeErrorKind::UnsupportedIndexRepresentation { rep: 1 }
        ),
        "unexpected error kind: {err:?}"
    );
}

#[test]
fn decodes_gs_instance_count_decl() {
    // dcl_gsinstancecount 4
    let body = vec![
        opcode_token(OPCODE_DCL_GS_INSTANCE_COUNT, 2),
        4,
        opcode_token(OPCODE_RET, 1),
    ];

    // Stage type 2 is geometry shader.
    let tokens = make_sm5_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Geometry);

    let module = decode_program(&program).expect("decode");
    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::GsInstanceCount { count: 4 })));
}

#[test]
fn decodes_integer_alu_and_conversions() {
    let mut body = Vec::<u32>::new();

    // iadd r0, r1, r2
    let mut iadd = vec![opcode_token(OPCODE_IADD, 1 + 2 + 2 + 2)];
    iadd.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    iadd.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    iadd.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&iadd);

    // imul r3, r0, l(3)
    let imm3 = imm32_scalar(3);
    let mut imul = vec![opcode_token(OPCODE_IMUL, (1 + 2 + 2 + imm3.len()) as u32)];
    imul.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    imul.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    imul.extend_from_slice(&imm3);
    body.extend_from_slice(&imul);

    // and r4, r0, r1
    let mut and = vec![opcode_token(OPCODE_AND, 1 + 2 + 2 + 2)];
    and.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    and.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    and.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&and);

    // or r5, r0, r1
    let mut or = vec![opcode_token(OPCODE_OR, 1 + 2 + 2 + 2)];
    or.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    or.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    or.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&or);

    // xor r6, r0, r1
    let mut xor = vec![opcode_token(OPCODE_XOR, 1 + 2 + 2 + 2)];
    xor.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    xor.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    xor.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&xor);

    // ishl r7, r0, l(1)
    let imm1 = imm32_scalar(1);
    let mut ishl = vec![opcode_token(OPCODE_ISHL, (1 + 2 + 2 + imm1.len()) as u32)];
    ishl.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 7, WriteMask::XYZW));
    ishl.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ishl.extend_from_slice(&imm1);
    body.extend_from_slice(&ishl);

    // ushr r8, r0, l(2)
    let imm2 = imm32_scalar(2);
    let mut ushr = vec![opcode_token(OPCODE_USHR, (1 + 2 + 2 + imm2.len()) as u32)];
    ushr.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 8, WriteMask::XYZW));
    ushr.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ushr.extend_from_slice(&imm2);
    body.extend_from_slice(&ushr);

    // ishr r9, r0, l(2)
    let mut ishr = vec![opcode_token(OPCODE_ISHR, (1 + 2 + 2 + imm2.len()) as u32)];
    ishr.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 9, WriteMask::XYZW));
    ishr.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ishr.extend_from_slice(&imm2);
    body.extend_from_slice(&ishr);

    // itof r10, r0
    let mut itof = vec![opcode_token(OPCODE_ITOF, 1 + 2 + 2)];
    itof.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 10, WriteMask::XYZW));
    itof.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&itof);

    // utof r11, r0
    let mut utof = vec![opcode_token(OPCODE_UTOF, 1 + 2 + 2)];
    utof.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 11, WriteMask::XYZW));
    utof.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&utof);

    // ftoi r12, l(1.5)
    let imm_f = imm32_scalar(1.5f32.to_bits());
    let mut ftoi = vec![opcode_token(OPCODE_FTOI, (1 + 2 + imm_f.len()) as u32)];
    ftoi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 12, WriteMask::XYZW));
    ftoi.extend_from_slice(&imm_f);
    body.extend_from_slice(&ftoi);

    // ftou r13, l(2.5)
    let imm_fu = imm32_scalar(2.5f32.to_bits());
    let mut ftou = vec![opcode_token(OPCODE_FTOU, (1 + 2 + imm_fu.len()) as u32)];
    ftou.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 13, WriteMask::XYZW));
    ftou.extend_from_slice(&imm_fu);
    body.extend_from_slice(&ftou);

    // movc r14, r0, r1, r2
    let mut movc = vec![opcode_token(OPCODE_MOVC, 1 + 2 + 2 + 2 + 2)];
    movc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 14, WriteMask::XYZW));
    movc.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    movc.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    movc.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&movc);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    let imm_scalar = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v, v, v, v]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };

    assert_eq!(
        module.instructions,
        vec![
            Sm4Inst::IAdd {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 1),
                b: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::IMul {
                dst_lo: dst(RegFile::Temp, 3, WriteMask::XYZW),
                dst_hi: None,
                a: src_reg(RegFile::Temp, 0),
                b: imm_scalar(3),
            },
            Sm4Inst::And {
                dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Or {
                dst: dst(RegFile::Temp, 5, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Xor {
                dst: dst(RegFile::Temp, 6, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::IShl {
                dst: dst(RegFile::Temp, 7, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: imm_scalar(1),
            },
            Sm4Inst::UShr {
                dst: dst(RegFile::Temp, 8, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: imm_scalar(2),
            },
            Sm4Inst::IShr {
                dst: dst(RegFile::Temp, 9, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: imm_scalar(2),
            },
            Sm4Inst::Itof {
                dst: dst(RegFile::Temp, 10, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Utof {
                dst: dst(RegFile::Temp, 11, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ftoi {
                dst: dst(RegFile::Temp, 12, WriteMask::XYZW),
                src: imm_scalar(1.5f32.to_bits()),
            },
            Sm4Inst::Ftou {
                dst: dst(RegFile::Temp, 13, WriteMask::XYZW),
                src: imm_scalar(2.5f32.to_bits()),
            },
            Sm4Inst::Movc {
                dst: dst(RegFile::Temp, 14, WriteMask::XYZW),
                cond: src_reg(RegFile::Temp, 0),
                a: src_reg(RegFile::Temp, 1),
                b: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Ret,
        ]
    );
}

#[test]
fn decodes_arithmetic_and_skips_decls() {
    const DCL_DUMMY: u32 = 0x100;

    let mut body = Vec::<u32>::new();

    // Declarations that should be captured before the instruction stream.
    // dcl_input v0.xyzw
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));
    // dcl_input_siv v1.xy, <sys_value=0x77>
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 1, 4)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 1, WriteMask(0b0011)));
    body.push(0x77);
    // dcl_output o0.xyzw
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 2, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    // dcl_output_siv o1.xyzw, <sys_value=0x88>
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 3, 4)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    body.push(0x88);
    // dcl_constantbuffer cb0[4]
    let cb_decl = reg_src(
        OPERAND_TYPE_CONSTANT_BUFFER,
        &[0, 4],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 4, 1 + cb_decl.len() as u32 + 1)]);
    body.extend_from_slice(&cb_decl);
    body.push(0); // access pattern token (ignored)
                  // dcl_resource_texture2d t0
    let tex_decl = reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 5, 1 + tex_decl.len() as u32 + 1)]);
    body.extend_from_slice(&tex_decl);
    body.push(2); // dimension token (ignored)
                  // dcl_sampler s0
    let samp_decl = reg_src(
        OPERAND_TYPE_SAMPLER,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 6, 1 + samp_decl.len() as u32)]);
    body.extend_from_slice(&samp_decl);
    // Unknown declaration-like opcode (no operand token).
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 7, 2), 4]);

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
    let module = decode_program(&program).expect("decode");

    let f = |v: f32| v.to_bits();
    let mut add_dst = dst(RegFile::Temp, 1, WriteMask::XYZW);
    add_dst.saturate = true;
    assert_eq!(
        module,
        Sm4Module {
            stage: aero_d3d11::ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::Input {
                    reg: 0,
                    mask: WriteMask::XYZW,
                },
                Sm4Decl::InputSiv {
                    reg: 1,
                    mask: WriteMask(0b0011),
                    sys_value: 0x77,
                },
                Sm4Decl::Output {
                    reg: 0,
                    mask: WriteMask::XYZW,
                },
                Sm4Decl::OutputSiv {
                    reg: 1,
                    mask: WriteMask::XYZW,
                    sys_value: 0x88,
                },
                Sm4Decl::ConstantBuffer {
                    slot: 0,
                    reg_count: 4,
                },
                Sm4Decl::ResourceTexture2D { slot: 0 },
                Sm4Decl::Sampler { slot: 0 },
                Sm4Decl::Unknown {
                    opcode: DCL_DUMMY + 7
                },
            ],
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
fn does_not_misclassify_unknown_instruction_as_decl() {
    const DCL_DUMMY: u32 = 0x100;
    // Pick an opcode that is not implemented by our decoder, but is still in the
    // executable-instruction range (< 0x100).
    const OPCODE_UNKNOWN: u32 = 0x20;

    let mut body = Vec::<u32>::new();

    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));

    body.push(opcode_token(OPCODE_UNKNOWN, 1));

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

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.decls.len(), 1);
    assert_eq!(module.instructions.len(), 3);
    assert!(matches!(&module.instructions[1], Sm4Inst::Mov { .. }));
}

#[test]
fn skips_nop_without_ending_decl_section() {
    const DCL_DUMMY: u32 = 0x100;

    let mut body = Vec::<u32>::new();

    // A leading NOP should not prevent the decoder from collecting declarations.
    body.push(opcode_token(OPCODE_NOP, 1));

    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));

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

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.decls.len(), 1);
    assert_eq!(module.instructions.len(), 2);
    assert!(matches!(&module.instructions[0], Sm4Inst::Mov { .. }));
}

#[test]
fn skips_customdata_comment_without_ending_decl_section() {
    const DCL_DUMMY: u32 = 0x100;

    let mut body = Vec::<u32>::new();

    // Custom-data comment block: opcode + customdata class token.
    body.extend_from_slice(&[opcode_token(OPCODE_CUSTOMDATA, 2), 0]);

    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));

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

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    // Custom-data blocks are non-executable and should not end the declaration section; they are
    // preserved as metadata declarations.
    assert_eq!(module.decls.len(), 2);
    assert_eq!(module.instructions.len(), 2);
}

#[test]
fn preserves_non_comment_customdata_and_does_not_end_decl_section() {
    const DCL_DUMMY: u32 = 0x100;

    let mut body = Vec::<u32>::new();

    // Non-comment customdata block, commonly used for embedded immediate constant buffers.
    body.extend_from_slice(&[
        opcode_token(OPCODE_CUSTOMDATA, 5),
        CUSTOMDATA_CLASS_IMMEDIATE_CONSTANT_BUFFER,
        0x1111_1111,
        0x2222_2222,
        0x3333_3333,
    ]);

    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));

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

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::ImmediateConstantBuffer {
                dwords: vec![0x1111_1111, 0x2222_2222, 0x3333_3333],
            },
            Sm4Decl::Input {
                reg: 0,
                mask: WriteMask::XYZW,
            },
        ]
    );

    // Customdata should not appear as an executable instruction.
    assert_eq!(module.instructions.len(), 2);
    assert!(matches!(&module.instructions[0], Sm4Inst::Mov { .. }));
}

#[test]
fn decodes_output_depth_operand() {
    // Minimal ps_5_0:
    //   mov oDepth.x, l(0.25)
    //   ret
    let f = |v: f32| v.to_bits();

    let mut body = Vec::<u32>::new();

    // mov oDepth.x, l(0.25, 0.25, 0.25, 0.25)
    // (The `oDepth` operand has no index; the backend maps it to the signature-declared SV_Depth.)
    let mut mov = vec![opcode_token(OPCODE_MOV, 1 + 1 + 5)];
    mov.push(operand_token(
        OPERAND_TYPE_OUTPUT_DEPTH,
        2,
        OPERAND_SEL_MASK,
        WriteMask::X.0 as u32,
        0,
        false,
    ));
    mov.extend_from_slice(&imm32_vec4([f(0.25), f(0.25), f(0.25), f(0.25)]));
    body.extend_from_slice(&mov);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module,
        Sm4Module {
            stage: aero_d3d11::ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                Sm4Inst::Mov {
                    dst: dst(RegFile::OutputDepth, 0, WriteMask::X),
                    src: src_imm([f(0.25), f(0.25), f(0.25), f(0.25)]),
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
    let module = decode_program(&program).expect("decode");

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

    assert_eq!(module.model, ShaderModel { major: 5, minor: 0 });
    assert_eq!(module.decls, vec![Sm4Decl::Unknown { opcode: DCL_DUMMY }]);
}

#[test]
fn decodes_sample_via_structural_fallback() {
    const DCL_DUMMY: u32 = 0x280;
    // Use an opcode ID that is not currently recognized by the decoder so we can exercise
    // the structural `sample` fallback path.
    // Keep this below `DECLARATION_OPCODE_MIN` (0x100) so the decoder won't classify it as a
    // declaration and skip the instruction stream.
    const OPCODE_UNKNOWN_SAMPLE: u32 = 0x7f;

    let mut body = Vec::<u32>::new();

    // Decls to skip.
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 2), 1]);

    // Unknown-opcode sample: sample r0, v0, t0, s0
    let mut sample = vec![opcode_token(OPCODE_UNKNOWN_SAMPLE, 1 + 2 + 2 + 2 + 2)];
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

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.decls, vec![Sm4Decl::Unknown { opcode: DCL_DUMMY }]);
    assert!(matches!(&module.instructions[0], Sm4Inst::Sample { .. }));
}

#[test]
fn does_not_misclassify_scalar_resource_op_as_ld() {
    let mut body = Vec::<u32>::new();

    // `resinfo` has ld-like operand types but a scalar mip-level operand; ensure the structural `ld`
    // fallback does not misclassify it as a texture load.
    let coord = imm32_scalar(0f32.to_bits());
    let mut inst = vec![opcode_token(
        OPCODE_RESINFO,
        (1 + 2 + coord.len() + 2) as u32,
    )];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    inst.extend_from_slice(&coord);
    inst.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&inst);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(matches!(&module.instructions[0], Sm4Inst::ResInfo { .. }));
    assert!(!module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Ld { .. })));
}

#[test]
fn decodes_resinfo_texture2d() {
    let mut body = Vec::<u32>::new();

    // resinfo r0.xyzw, l(0), t3
    let lod = imm32_scalar(0f32.to_bits());
    let mut resinfo = vec![opcode_token(OPCODE_RESINFO, (1 + 2 + lod.len() + 2) as u32)];
    resinfo.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    resinfo.extend_from_slice(&lod);
    resinfo.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[3],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&resinfo);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 is compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::ResInfo {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            texture: TextureRef { slot: 3 },
            mip_level: SrcOperand {
                kind: SrcKind::ImmediateF32([0f32.to_bits(); 4]),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
        }
    );
    assert_eq!(module.stage, aero_d3d11::ShaderStage::Compute);
}

#[test]
fn decodes_ld_texture_load() {
    let mut body = Vec::<u32>::new();

    // ld r0, r1, t0
    let mut ld = vec![opcode_token(OPCODE_LD, 1 + 2 + 2 + 2)];
    ld.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ld);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Ld {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            coord: src_reg(RegFile::Temp, 1),
            texture: TextureRef { slot: 0 },
            lod: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1
                }),
                swizzle: Swizzle::ZZZZ,
                modifier: OperandModifier::None,
            },
        }
    );
}

#[test]
fn decodes_ld_uav_raw() {
    let mut body = Vec::<u32>::new();

    // ld_uav_raw r0, r1.x, u0
    let mut ld = vec![opcode_token(OPCODE_LD_UAV_RAW, 1 + 2 + 2 + 2)];
    ld.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    ));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ld);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::LdUavRaw {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            addr: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            uav: UavRef { slot: 0 },
        }
    );
}

#[test]
fn decodes_ld_via_structural_fallback() {
    const DCL_DUMMY: u32 = 0x300;
    // Use an opcode ID that is not currently recognized by the decoder so we can exercise the
    // structural `ld` fallback path.
    const OPCODE_UNKNOWN_LD: u32 = 0x4a;

    let mut body = Vec::<u32>::new();

    // Decls to skip.
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 2), 1]);

    // Unknown-opcode texture load: ld r0, v0, t0
    let mut ld = vec![opcode_token(OPCODE_UNKNOWN_LD, 1 + 2 + 2 + 2)];
    ld.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_INPUT,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ld);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.decls, vec![Sm4Decl::Unknown { opcode: DCL_DUMMY }]);
    assert_eq!(
        module.instructions[0],
        Sm4Inst::Ld {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            coord: src_reg(RegFile::Input, 0),
            texture: TextureRef { slot: 0 },
            lod: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Input,
                    index: 0,
                }),
                swizzle: Swizzle::ZZZZ,
                modifier: OperandModifier::None,
            },
        }
    );
}

#[test]
fn decodes_ld_with_explicit_lod_operand() {
    let mut body = Vec::<u32>::new();

    // ld r0, r1, t0, l(5)
    let explicit_lod = imm32_scalar(5);
    let mut ld = vec![opcode_token(
        OPCODE_LD,
        (1 + 2 + 2 + 2 + explicit_lod.len()) as u32,
    )];
    ld.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ld.extend_from_slice(&explicit_lod);
    body.extend_from_slice(&ld);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Ld {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            coord: src_reg(RegFile::Temp, 1),
            texture: TextureRef { slot: 0 },
            lod: SrcOperand {
                kind: SrcKind::ImmediateF32([5, 5, 5, 5]),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
        }
    );
}

#[test]
fn decodes_integer_and_bitwise_ops() {
    let mut body = Vec::<u32>::new();

    // iadd r0, r1, r2
    let mut iadd = vec![opcode_token(OPCODE_IADD, 1 + 2 + 2 + 2)];
    iadd.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    iadd.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    iadd.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&iadd);

    // isub r3, r0, r2
    let mut isub = vec![opcode_token(OPCODE_ISUB, 1 + 2 + 2 + 2)];
    isub.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    isub.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    isub.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&isub);

    // imul r4, r3, r1
    let mut imul = vec![opcode_token(OPCODE_IMUL, 1 + 2 + 2 + 2)];
    imul.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    imul.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[3],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    imul.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&imul);

    // and r5, r4, r0
    let mut and = vec![opcode_token(OPCODE_AND, 1 + 2 + 2 + 2)];
    and.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    and.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[4],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    and.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&and);

    // or r6, r4, r0
    let mut or = vec![opcode_token(OPCODE_OR, 1 + 2 + 2 + 2)];
    or.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    or.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[4],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    or.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&or);

    // xor r7, r4, r0
    let mut xor = vec![opcode_token(OPCODE_XOR, 1 + 2 + 2 + 2)];
    xor.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 7, WriteMask::XYZW));
    xor.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[4],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    xor.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&xor);

    // not r8, r7
    let mut not = vec![opcode_token(OPCODE_NOT, 1 + 2 + 2)];
    not.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 8, WriteMask::XYZW));
    not.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[7],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&not);

    // ishl r9, r8, r1
    let mut ishl = vec![opcode_token(OPCODE_ISHL, 1 + 2 + 2 + 2)];
    ishl.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 9, WriteMask::XYZW));
    ishl.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[8],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ishl.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ishl);

    // ishr r10, r8, r1
    let mut ishr = vec![opcode_token(OPCODE_ISHR, 1 + 2 + 2 + 2)];
    ishr.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 10, WriteMask::XYZW));
    ishr.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[8],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ishr.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ishr);

    // ushr r11, r8, r1
    let mut ushr = vec![opcode_token(OPCODE_USHR, 1 + 2 + 2 + 2)];
    ushr.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 11, WriteMask::XYZW));
    ushr.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[8],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ushr.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ushr);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = aero_d3d11::sm4::decode_program(&program).expect("decode");

    assert_eq!(module.instructions.len(), 11);
    assert!(matches!(&module.instructions[0], Sm4Inst::IAdd { .. }));
    assert!(matches!(&module.instructions[1], Sm4Inst::ISub { .. }));
    assert!(matches!(&module.instructions[2], Sm4Inst::IMul { .. }));
    assert!(matches!(&module.instructions[3], Sm4Inst::And { .. }));
    assert!(matches!(&module.instructions[4], Sm4Inst::Or { .. }));
    assert!(matches!(&module.instructions[5], Sm4Inst::Xor { .. }));
    assert!(matches!(&module.instructions[6], Sm4Inst::Not { .. }));
    assert!(matches!(&module.instructions[7], Sm4Inst::IShl { .. }));
    assert!(matches!(&module.instructions[8], Sm4Inst::IShr { .. }));
    assert!(matches!(&module.instructions[9], Sm4Inst::UShr { .. }));
    assert!(matches!(&module.instructions[10], Sm4Inst::Ret));
}

#[test]
fn does_not_decode_ld_with_offset_like_trailing_operand_as_explicit_lod() {
    let mut body = Vec::<u32>::new();

    // ld r0, r1, t0, r2.xy (this resembles `Texture2D.Load(..., offset)` which is not implemented).
    let offset = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ld = vec![opcode_token(
        OPCODE_LD,
        (1 + 2 + 2 + 2 + offset.len()) as u32,
    )];
    ld.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ld.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    ld.extend_from_slice(&offset);
    body.extend_from_slice(&ld);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(matches!(
        module.instructions[0],
        Sm4Inst::Unknown { opcode: OPCODE_LD }
    ));
}

#[test]
fn decodes_ubfe_ibfe_bfi_bitfield_ops() {
    let mut body = Vec::<u32>::new();

    // ubfe r0, l(8), l(0), r1
    let width = imm32_scalar(8);
    let offset = imm32_scalar(0);
    let src = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ubfe = vec![opcode_token(
        OPCODE_UBFE,
        (1 + 2 + width.len() + offset.len() + src.len()) as u32,
    )];
    ubfe.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ubfe.extend_from_slice(&width);
    ubfe.extend_from_slice(&offset);
    ubfe.extend_from_slice(&src);
    body.extend_from_slice(&ubfe);

    // ibfe r2, l(8), l(0), r3
    let width = imm32_scalar(8);
    let offset = imm32_scalar(0);
    let src = reg_src(
        OPERAND_TYPE_TEMP,
        &[3],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ibfe = vec![opcode_token(
        OPCODE_IBFE,
        (1 + 2 + width.len() + offset.len() + src.len()) as u32,
    )];
    ibfe.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    ibfe.extend_from_slice(&width);
    ibfe.extend_from_slice(&offset);
    ibfe.extend_from_slice(&src);
    body.extend_from_slice(&ibfe);

    // bfi r4, l(8), l(0), r5, r6
    let width = imm32_scalar(8);
    let offset = imm32_scalar(0);
    let insert = reg_src(
        OPERAND_TYPE_TEMP,
        &[5],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let base = reg_src(
        OPERAND_TYPE_TEMP,
        &[6],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut bfi = vec![opcode_token(
        OPCODE_BFI,
        (1 + 2 + width.len() + offset.len() + insert.len() + base.len()) as u32,
    )];
    bfi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    bfi.extend_from_slice(&width);
    bfi.extend_from_slice(&offset);
    bfi.extend_from_slice(&insert);
    bfi.extend_from_slice(&base);
    body.extend_from_slice(&bfi);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    let imm_scalar = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v, v, v, v]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Ubfe {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            width: imm_scalar(8),
            offset: imm_scalar(0),
            src: src_reg(RegFile::Temp, 1),
        }
    );
    assert_eq!(
        module.instructions[1],
        Sm4Inst::Ibfe {
            dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
            width: imm_scalar(8),
            offset: imm_scalar(0),
            src: src_reg(RegFile::Temp, 3),
        }
    );
    assert_eq!(
        module.instructions[2],
        Sm4Inst::Bfi {
            dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
            width: imm_scalar(8),
            offset: imm_scalar(0),
            insert: src_reg(RegFile::Temp, 5),
            base: src_reg(RegFile::Temp, 6),
        }
    );
}

#[test]
fn decodes_integer_compare_ops() {
    let mut body = Vec::<u32>::new();

    // ieq r0, r1, r2
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ieq = vec![opcode_token(OPCODE_IEQ, (1 + 2 + a.len() + b.len()) as u32)];
    ieq.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ieq.extend_from_slice(&a);
    ieq.extend_from_slice(&b);
    body.extend_from_slice(&ieq);

    // ine r3, r4, r5
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[4],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[5],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ine = vec![opcode_token(OPCODE_INE, (1 + 2 + a.len() + b.len()) as u32)];
    ine.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    ine.extend_from_slice(&a);
    ine.extend_from_slice(&b);
    body.extend_from_slice(&ine);

    // ilt r6, r7, r8
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[7],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[8],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ilt = vec![opcode_token(OPCODE_ILT, (1 + 2 + a.len() + b.len()) as u32)];
    ilt.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    ilt.extend_from_slice(&a);
    ilt.extend_from_slice(&b);
    body.extend_from_slice(&ilt);

    // ige r9, r10, r11
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[10],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[11],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ige = vec![opcode_token(OPCODE_IGE, (1 + 2 + a.len() + b.len()) as u32)];
    ige.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 9, WriteMask::XYZW));
    ige.extend_from_slice(&a);
    ige.extend_from_slice(&b);
    body.extend_from_slice(&ige);

    // ult r12, r13, r14
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[13],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[14],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut ult = vec![opcode_token(OPCODE_ULT, (1 + 2 + a.len() + b.len()) as u32)];
    ult.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 12, WriteMask::XYZW));
    ult.extend_from_slice(&a);
    ult.extend_from_slice(&b);
    body.extend_from_slice(&ult);

    // uge r15, r16, r17
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[16],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[17],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut uge = vec![opcode_token(OPCODE_UGE, (1 + 2 + a.len() + b.len()) as u32)];
    uge.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 15, WriteMask::XYZW));
    uge.extend_from_slice(&a);
    uge.extend_from_slice(&b);
    body.extend_from_slice(&uge);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 1),
            b: src_reg(RegFile::Temp, 2),
            op: aero_d3d11::CmpOp::Eq,
            ty: aero_d3d11::CmpType::I32,
        }
    );
    assert_eq!(
        module.instructions[1],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 4),
            b: src_reg(RegFile::Temp, 5),
            op: aero_d3d11::CmpOp::Ne,
            ty: aero_d3d11::CmpType::I32,
        }
    );
    assert_eq!(
        module.instructions[2],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 6, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 7),
            b: src_reg(RegFile::Temp, 8),
            op: aero_d3d11::CmpOp::Lt,
            ty: aero_d3d11::CmpType::I32,
        }
    );
    assert_eq!(
        module.instructions[3],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 9, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 10),
            b: src_reg(RegFile::Temp, 11),
            op: aero_d3d11::CmpOp::Ge,
            ty: aero_d3d11::CmpType::I32,
        }
    );
    assert_eq!(
        module.instructions[4],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 12, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 13),
            b: src_reg(RegFile::Temp, 14),
            op: aero_d3d11::CmpOp::Lt,
            ty: aero_d3d11::CmpType::U32,
        }
    );
    assert_eq!(
        module.instructions[5],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 15, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 16),
            b: src_reg(RegFile::Temp, 17),
            op: aero_d3d11::CmpOp::Ge,
            ty: aero_d3d11::CmpType::U32,
        }
    );
}

#[test]
fn decodes_float_compare_ops() {
    let mut body = Vec::<u32>::new();

    // eq r0, r1, r2
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut eq = vec![opcode_token(OPCODE_EQ, (1 + 2 + a.len() + b.len()) as u32)];
    eq.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    eq.extend_from_slice(&a);
    eq.extend_from_slice(&b);
    body.extend_from_slice(&eq);

    // le r3, r4, r5
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[4],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[5],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut le = vec![opcode_token(OPCODE_LE, (1 + 2 + a.len() + b.len()) as u32)];
    le.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    le.extend_from_slice(&a);
    le.extend_from_slice(&b);
    body.extend_from_slice(&le);

    // gt r6, r7, r8
    let a = reg_src(
        OPERAND_TYPE_TEMP,
        &[7],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let b = reg_src(
        OPERAND_TYPE_TEMP,
        &[8],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut gt = vec![opcode_token(OPCODE_GT, (1 + 2 + a.len() + b.len()) as u32)];
    gt.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    gt.extend_from_slice(&a);
    gt.extend_from_slice(&b);
    body.extend_from_slice(&gt);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 1),
            b: src_reg(RegFile::Temp, 2),
            op: aero_d3d11::CmpOp::Eq,
            ty: aero_d3d11::CmpType::F32,
        }
    );
    assert_eq!(
        module.instructions[1],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 4),
            b: src_reg(RegFile::Temp, 5),
            op: aero_d3d11::CmpOp::Le,
            ty: aero_d3d11::CmpType::F32,
        }
    );
    assert_eq!(
        module.instructions[2],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 6, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 7),
            b: src_reg(RegFile::Temp, 8),
            op: aero_d3d11::CmpOp::Gt,
            ty: aero_d3d11::CmpType::F32,
        }
    );
}

#[test]
fn decodes_sync_with_thread_group_sync() {
    let mut body = Vec::<u32>::new();

    // `sync` encodes flags in bits 24..=30 of the opcode token.
    let flags = SYNC_FLAG_THREAD_GROUP_SYNC | SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY;
    body.push(opcode_token(OPCODE_SYNC, 1) | (flags << OPCODE_CONTROL_SHIFT));
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 is compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.stage, aero_d3d11::ShaderStage::Compute);
    assert_eq!(module.model, ShaderModel { major: 5, minor: 0 });
    assert_eq!(module.instructions[0], Sm4Inst::Sync { flags });
}

#[test]
fn decodes_sync_with_uav_memory_without_thread_group_sync() {
    let mut body = Vec::<u32>::new();

    // UAV/storage memory fence without a thread-group barrier.
    let flags = SYNC_FLAG_UAV_MEMORY;
    body.push(opcode_token(OPCODE_SYNC, 1) | (flags << OPCODE_CONTROL_SHIFT));
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 is compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.instructions[0], Sm4Inst::Sync { flags });
}

#[test]
fn decodes_bit_utils_ops() {
    let mut body = Vec::<u32>::new();

    // bfrev r0, r1
    let src = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut bfrev = vec![opcode_token(OPCODE_BFREV, (1 + 2 + src.len()) as u32)];
    bfrev.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    bfrev.extend_from_slice(&src);
    body.extend_from_slice(&bfrev);

    // countbits r2, r3
    let src = reg_src(
        OPERAND_TYPE_TEMP,
        &[3],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut countbits = vec![opcode_token(OPCODE_COUNTBITS, (1 + 2 + src.len()) as u32)];
    countbits.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    countbits.extend_from_slice(&src);
    body.extend_from_slice(&countbits);

    // firstbit_hi r4, r5
    let src = reg_src(
        OPERAND_TYPE_TEMP,
        &[5],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut firstbit_hi = vec![opcode_token(OPCODE_FIRSTBIT_HI, (1 + 2 + src.len()) as u32)];
    firstbit_hi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    firstbit_hi.extend_from_slice(&src);
    body.extend_from_slice(&firstbit_hi);

    // firstbit_lo r6, r7
    let src = reg_src(
        OPERAND_TYPE_TEMP,
        &[7],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut firstbit_lo = vec![opcode_token(OPCODE_FIRSTBIT_LO, (1 + 2 + src.len()) as u32)];
    firstbit_lo.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    firstbit_lo.extend_from_slice(&src);
    body.extend_from_slice(&firstbit_lo);

    // firstbit_shi r8, r9
    let src = reg_src(
        OPERAND_TYPE_TEMP,
        &[9],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut firstbit_shi = vec![opcode_token(
        OPCODE_FIRSTBIT_SHI,
        (1 + 2 + src.len()) as u32,
    )];
    firstbit_shi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 8, WriteMask::XYZW));
    firstbit_shi.extend_from_slice(&src);
    body.extend_from_slice(&firstbit_shi);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Bfrev {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            src: src_reg(RegFile::Temp, 1),
        }
    );
    assert_eq!(
        module.instructions[1],
        Sm4Inst::CountBits {
            dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
            src: src_reg(RegFile::Temp, 3),
        }
    );
    assert_eq!(
        module.instructions[2],
        Sm4Inst::FirstbitHi {
            dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
            src: src_reg(RegFile::Temp, 5),
        }
    );
    assert_eq!(
        module.instructions[3],
        Sm4Inst::FirstbitLo {
            dst: dst(RegFile::Temp, 6, WriteMask::XYZW),
            src: src_reg(RegFile::Temp, 7),
        }
    );
    assert_eq!(
        module.instructions[4],
        Sm4Inst::FirstbitShi {
            dst: dst(RegFile::Temp, 8, WriteMask::XYZW),
            src: src_reg(RegFile::Temp, 9),
        }
    );
    assert_eq!(module.instructions[5], Sm4Inst::Ret);
}

#[test]
fn decodes_cmp_movc_and_structured_if() {
    let mut body = Vec::<u32>::new();

    // lt r0, v0.xxxx, l(1.0)
    let imm_one = imm32_scalar(1.0f32.to_bits());
    let mut lt = vec![opcode_token(OPCODE_LT, (1 + 2 + 2 + imm_one.len()) as u32)];
    lt.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    lt.extend_from_slice(&reg_src(
        OPERAND_TYPE_INPUT,
        &[0],
        Swizzle::XXXX,
        OperandModifier::None,
    ));
    lt.extend_from_slice(&imm_one);
    body.extend_from_slice(&lt);

    // movc r1, r0, l(1,0,0,1), l(0,0,1,1)
    let t = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    let f = imm32_vec4([
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    let mut movc = vec![opcode_token(
        OPCODE_MOVC,
        (1 + 2 + 2 + t.len() + f.len()) as u32,
    )];
    movc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    movc.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    movc.extend_from_slice(&t);
    movc.extend_from_slice(&f);
    body.extend_from_slice(&movc);

    // if_nz r0.x
    let mut if_inst = vec![opcode_token(OPCODE_IF, 1 + 2) | (1 << OPCODE_TEST_BOOLEAN_SHIFT)];
    if_inst.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XXXX,
        OperandModifier::None,
    ));
    body.extend_from_slice(&if_inst);

    // mov r1.y, l(1.0)
    let imm_one = imm32_scalar(1.0f32.to_bits());
    let mut mov_then = vec![opcode_token(OPCODE_MOV, (1 + 2 + imm_one.len()) as u32)];
    mov_then.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::Y));
    mov_then.extend_from_slice(&imm_one);
    body.extend_from_slice(&mov_then);

    // else
    body.push(opcode_token(OPCODE_ELSE, 1));

    // mov r1.x, l(2.0)
    let imm_two = imm32_scalar(2.0f32.to_bits());
    let mut mov_else = vec![opcode_token(OPCODE_MOV, (1 + 2 + imm_two.len()) as u32)];
    mov_else.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::X));
    mov_else.extend_from_slice(&imm_two);
    body.extend_from_slice(&mov_else);

    // endif
    body.push(opcode_token(OPCODE_ENDIF, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    let imm_scalar = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v, v, v, v]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            op: aero_d3d11::CmpOp::Lt,
            ty: aero_d3d11::CmpType::F32,
            a: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Input,
                    index: 0,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            b: imm_scalar(1.0f32.to_bits()),
        }
    );
    assert_eq!(
        module.instructions[1],
        Sm4Inst::Movc {
            dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
            cond: src_reg(RegFile::Temp, 0),
            a: SrcOperand {
                kind: SrcKind::ImmediateF32([
                    1.0f32.to_bits(),
                    0.0f32.to_bits(),
                    0.0f32.to_bits(),
                    1.0f32.to_bits(),
                ]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            b: SrcOperand {
                kind: SrcKind::ImmediateF32([
                    0.0f32.to_bits(),
                    0.0f32.to_bits(),
                    1.0f32.to_bits(),
                    1.0f32.to_bits(),
                ]),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }
    );

    assert_eq!(
        module.instructions[2],
        Sm4Inst::If {
            cond: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 0,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            test: aero_d3d11::Sm4TestBool::NonZero,
        }
    );
    assert_eq!(
        module.instructions[3],
        Sm4Inst::Mov {
            dst: dst(RegFile::Temp, 1, WriteMask::Y),
            src: imm_scalar(1.0f32.to_bits()),
        }
    );
    assert_eq!(module.instructions[4], Sm4Inst::Else);
    assert_eq!(
        module.instructions[5],
        Sm4Inst::Mov {
            dst: dst(RegFile::Temp, 1, WriteMask::X),
            src: imm_scalar(2.0f32.to_bits()),
        }
    );
    assert_eq!(module.instructions[6], Sm4Inst::EndIf);
    assert_eq!(module.instructions[7], Sm4Inst::Ret);
}

#[test]
fn decodes_udiv_and_idiv_with_two_dest_operands() {
    let mut body = Vec::<u32>::new();

    // udiv r0, r1, r2, r3
    let mut udiv = vec![opcode_token(OPCODE_UDIV, 1 + 2 + 2 + 2 + 2)];
    udiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    udiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    udiv.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    udiv.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[3],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&udiv);

    // idiv r4, r5, r6, r7
    let mut idiv = vec![opcode_token(OPCODE_IDIV, 1 + 2 + 2 + 2 + 2)];
    idiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    idiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    idiv.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[6],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    idiv.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[7],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&idiv);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(matches!(
        &module.instructions[0],
        Sm4Inst::UDiv {
            dst_quot,
            dst_rem,
            ..
        } if dst_quot.reg.index == 0 && dst_rem.reg.index == 1
    ));
    assert!(matches!(
        &module.instructions[1],
        Sm4Inst::IDiv {
            dst_quot,
            dst_rem,
            ..
        } if dst_quot.reg.index == 4 && dst_rem.reg.index == 5
    ));
}

#[test]
fn decodes_integer_minmax_abs_neg_ops() {
    let mut body = Vec::<u32>::new();

    // imin r0, r1, r2
    let mut imin = vec![opcode_token(OPCODE_IMIN, 1 + 2 + 2 + 2)];
    imin.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    imin.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    imin.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&imin);

    // imax r3, r4, r5
    let mut imax = vec![opcode_token(OPCODE_IMAX, 1 + 2 + 2 + 2)];
    imax.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    imax.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[4],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    imax.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[5],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&imax);

    // umin r6, r7, r8
    let mut umin = vec![opcode_token(OPCODE_UMIN, 1 + 2 + 2 + 2)];
    umin.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    umin.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[7],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    umin.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[8],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&umin);

    // umax r9, r10, r11
    let mut umax = vec![opcode_token(OPCODE_UMAX, 1 + 2 + 2 + 2)];
    umax.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 9, WriteMask::XYZW));
    umax.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[10],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    umax.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[11],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&umax);

    // iabs r12, r13
    let mut iabs = vec![opcode_token(OPCODE_IABS, 1 + 2 + 2)];
    iabs.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 12, WriteMask::XYZW));
    iabs.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[13],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&iabs);

    // ineg r14, r15
    let mut ineg = vec![opcode_token(OPCODE_INEG, 1 + 2 + 2)];
    ineg.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 14, WriteMask::XYZW));
    ineg.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[15],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ineg);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions,
        vec![
            Sm4Inst::IMin {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 1),
                b: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::IMax {
                dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 4),
                b: src_reg(RegFile::Temp, 5),
            },
            Sm4Inst::UMin {
                dst: dst(RegFile::Temp, 6, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 7),
                b: src_reg(RegFile::Temp, 8),
            },
            Sm4Inst::UMax {
                dst: dst(RegFile::Temp, 9, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 10),
                b: src_reg(RegFile::Temp, 11),
            },
            Sm4Inst::IAbs {
                dst: dst(RegFile::Temp, 12, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 13),
            },
            Sm4Inst::INeg {
                dst: dst(RegFile::Temp, 14, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 15),
            },
            Sm4Inst::Ret,
        ]
    );
}

#[test]
fn sm5_uav_and_raw_buffer_opcode_constants_are_stable() {
    // These constants are used by compute/UAV decoding work; treat this test as a guard against
    // accidental renumbering.
    //
    // Note: these opcode IDs are specific to Aero's current SM4 token encoding used by
    // fixtures/tests, and are *not* the Windows SDK `D3D11_SB_OPCODE_TYPE` values.
    assert_eq!(OPERAND_TYPE_NULL, 13);
    assert_eq!(OPERAND_TYPE_UNORDERED_ACCESS_VIEW, 30);

    // Predicate ops.
    assert_eq!(OPCODE_SETP, 0x2c);

    // Float compare opcodes.
    assert_eq!(OPCODE_LT, 0x0c);
    assert_eq!(OPCODE_GE, 0x0d);
    assert_eq!(OPCODE_EQ, 0x0e);
    assert_eq!(OPCODE_NE, 0x0f);
    assert_eq!(OPCODE_GT, 0x10);
    assert_eq!(OPCODE_LE, 0x11);

    // Numeric conversion opcodes.
    assert_eq!(OPCODE_FTOI, 0x18);
    assert_eq!(OPCODE_FTOU, 0x19);
    assert_eq!(OPCODE_ITOF, 0x1a);
    assert_eq!(OPCODE_UTOF, 0x1b);
    assert_eq!(OPCODE_F32TOF16, 0x5d);
    assert_eq!(OPCODE_F16TOF32, 0x5e);
    // Structured control flow opcodes.
    assert_eq!(OPCODE_BREAK, 0x2d);
    assert_eq!(OPCODE_BREAKC, 0x2e);
    assert_eq!(OPCODE_LOOP, 0x2f);
    assert_eq!(OPCODE_ENDLOOP, 0x30);
    assert_eq!(OPCODE_CONTINUE, 0x31);
    assert_eq!(OPCODE_CONTINUEC, 0x32);
    assert_eq!(OPCODE_SWITCH, 0x35);
    assert_eq!(OPCODE_CASE, 0x36);
    assert_eq!(OPCODE_DEFAULT, 0x37);
    assert_eq!(OPCODE_ENDSWITCH, 0x38);
    // Integer arithmetic opcodes.
    assert_eq!(OPCODE_IABS, 0x61);
    assert_eq!(OPCODE_INEG, 0x62);
    assert_eq!(OPCODE_IMIN, 0x63);
    assert_eq!(OPCODE_IMAX, 0x64);
    assert_eq!(OPCODE_UMIN, 0x65);
    assert_eq!(OPCODE_UMAX, 0x66);
    // Integer/bitwise ops.
    assert_eq!(OPCODE_IADD, 0x1d);
    assert_eq!(OPCODE_ISUB, 0x1e);
    assert_eq!(OPCODE_IMUL, 0x70);
    assert_eq!(OPCODE_AND, 0x71);
    assert_eq!(OPCODE_OR, 0x72);
    assert_eq!(OPCODE_XOR, 0x23);
    assert_eq!(OPCODE_NOT, 0x24);
    assert_eq!(OPCODE_ISHL, 0x25);
    assert_eq!(OPCODE_ISHR, 0x26);
    assert_eq!(OPCODE_USHR, 0x73);
    // Integer compare opcodes.
    assert_eq!(OPCODE_IEQ, 0x4d);
    assert_eq!(OPCODE_IGE, 0x4e);
    assert_eq!(OPCODE_ILT, 0x4f);
    assert_eq!(OPCODE_INE, 0x50);
    assert_eq!(OPCODE_ULT, 0x51);
    assert_eq!(OPCODE_UGE, 0x52);
    // Compare opcodes must not overlap the integer/bitwise opcode range; the decoder relies on
    // unique opcode IDs for dispatch.
    let int_bitwise_opcodes = [
        OPCODE_IADD,
        OPCODE_ISUB,
        OPCODE_IMUL,
        OPCODE_AND,
        OPCODE_OR,
        OPCODE_XOR,
        OPCODE_NOT,
        OPCODE_ISHL,
        OPCODE_ISHR,
        OPCODE_USHR,
    ];
    let int_bitwise_set = int_bitwise_opcodes
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        int_bitwise_set.len(),
        int_bitwise_opcodes.len(),
        "duplicate integer/bitwise opcode"
    );
    let compare_opcodes = [
        OPCODE_IEQ, OPCODE_INE, OPCODE_ILT, OPCODE_IGE, OPCODE_ULT, OPCODE_UGE,
    ];
    for (i, &opcode) in compare_opcodes.iter().enumerate() {
        assert!(
            !int_bitwise_opcodes.contains(&opcode),
            "compare opcode {opcode:#x} overlaps integer/bitwise opcode table"
        );
        for &other in &compare_opcodes[..i] {
            assert_ne!(opcode, other, "duplicate compare opcode {opcode:#x}");
        }
    }
    assert_eq!(OPCODE_DCL_INPUT, 0x100);
    assert_eq!(OPCODE_DCL_OUTPUT, 0x101);
    assert_eq!(OPCODE_DCL_THREAD_GROUP, 0x11f);
    assert_eq!(OPCODE_DCL_RESOURCE, 0x102);
    assert_eq!(OPCODE_DCL_SAMPLER, 0x103);
    assert_eq!(OPCODE_DCL_RESOURCE_RAW, 0x205);
    assert_eq!(OPCODE_DCL_RESOURCE_STRUCTURED, 0x206);
    assert_eq!(OPCODE_DCL_UAV_RAW, 0x207);
    assert_eq!(OPCODE_DCL_UAV_STRUCTURED, 0x208);
    assert_eq!(OPCODE_LD_RAW, 0x53);
    assert_eq!(OPCODE_LD_STRUCTURED, 0x54);
    assert_eq!(OPCODE_LD_UAV_RAW, 0x6f);
    assert_eq!(OPCODE_STORE_RAW, 0x56);
    assert_eq!(OPCODE_STORE_STRUCTURED, 0x57);
    // Structured control flow.
    assert_eq!(OPCODE_IFC, 0x2b);
    assert_eq!(OPCODE_BREAKC, 0x2e);
    assert_eq!(OPCODE_LOOP, 0x2f);
    assert_eq!(OPCODE_ENDLOOP, 0x30);
    assert_eq!(OPCODE_CONTINUE, 0x31);
    assert_eq!(OPCODE_CONTINUEC, 0x32);
}

#[test]
fn sm5_tessellation_decl_opcode_constants_are_stable() {
    // These opcode IDs are part of Aero's SM4 token encoding used by fixtures/tests.
    // They are *not* the Windows SDK `D3D11_SB_OPCODE_TYPE` values.
    assert_eq!(OPCODE_DCL_HS_MAX_TESSFACTOR, 0x110);
    assert_eq!(OPCODE_DCL_HS_DOMAIN, 0x113);
    assert_eq!(OPCODE_DCL_HS_PARTITIONING, 0x114);
    assert_eq!(OPCODE_DCL_HS_OUTPUT_TOPOLOGY, 0x115);
    assert_eq!(OPCODE_DCL_HS_OUTPUT_CONTROL_POINT_COUNT, 0x116);
    assert_eq!(OPCODE_DCL_DS_DOMAIN, 0x119);
}

#[test]
fn decodes_sm5_hs_tessellation_declarations() {
    // hs_5_0 with only tessellation-related declarations:
    // - dcl_hs_domain tri
    // - dcl_hs_partitioning integer
    // - dcl_hs_output_topology triangle_cw
    // - dcl_hs_output_control_point_count 3
    // - dcl_hs_max_tessfactor 64.0
    // - ret
    let mut body = Vec::<u32>::new();

    body.extend_from_slice(&[opcode_token(OPCODE_DCL_HS_DOMAIN, 2), 2]);
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_HS_PARTITIONING, 2), 1]);
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_HS_OUTPUT_TOPOLOGY, 2), 3]);
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_HS_OUTPUT_CONTROL_POINT_COUNT, 2), 3]);
    body.extend_from_slice(&[
        opcode_token(OPCODE_DCL_HS_MAX_TESSFACTOR, 2),
        64.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 3 is hull shader.
    let tokens = make_sm5_program_tokens(3, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Hull);

    let module = decode_program(&program).expect("decode");
    assert_eq!(module.stage, aero_d3d11::ShaderStage::Hull);

    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::HsDomain {
                domain: HsDomain::Tri
            },
            Sm4Decl::HsPartitioning {
                partitioning: HsPartitioning::Integer
            },
            Sm4Decl::HsOutputTopology {
                topology: HsOutputTopology::TriangleCw
            },
            Sm4Decl::HsOutputControlPointCount { count: 3 },
            Sm4Decl::HsMaxTessFactor {
                factor: 64.0f32.to_bits()
            },
        ]
    );
    assert_eq!(module.instructions, vec![Sm4Inst::Ret]);
}

#[test]
fn decodes_sm5_ds_domain_declaration() {
    // ds_5_0 with just dcl_ds_domain tri; ret.
    let body = [
        opcode_token(OPCODE_DCL_DS_DOMAIN, 2),
        2,
        opcode_token(OPCODE_RET, 1),
    ];

    // Stage type 4 is domain shader.
    let tokens = make_sm5_program_tokens(4, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Domain);

    let module = decode_program(&program).expect("decode");
    assert_eq!(
        module.decls,
        vec![Sm4Decl::DsDomain {
            domain: HsDomain::Tri
        }]
    );
    assert_eq!(module.instructions, vec![Sm4Inst::Ret]);
}

#[test]
fn decodes_loop_endloop_and_break() {
    let body = [
        opcode_token(OPCODE_LOOP, 1),
        opcode_token(OPCODE_BREAK, 1),
        opcode_token(OPCODE_ENDLOOP, 1),
        opcode_token(OPCODE_RET, 1),
    ];
    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions,
        vec![
            Sm4Inst::Loop,
            Sm4Inst::Break,
            Sm4Inst::EndLoop,
            Sm4Inst::Ret
        ]
    );
}

#[test]
fn decodes_sm5_tessellation_decls() {
    let mut body = Vec::<u32>::new();

    // dcl_inputcontrolpoints 3
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2), 3]);
    // dcl_outputcontrolpoints 4
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_HS_OUTPUT_CONTROL_POINT_COUNT, 2), 4]);
    // dcl_tessellator_domain tri
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_TESS_DOMAIN, 2), 2]);
    // dcl_tessellator_partitioning fractional_even
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_TESS_PARTITIONING, 2), 4]);
    // dcl_tessellator_output_primitive triangle_cw
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_TESS_OUTPUT_PRIMITIVE, 2), 3]);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 3 is hull shader.
    let tokens = make_sm5_program_tokens(3, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Hull);

    let module = decode_program(&program).expect("decode");
    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::InputControlPointCount { count: 3 },
            Sm4Decl::HsOutputControlPointCount { count: 4 },
            Sm4Decl::HsDomain {
                domain: HsDomain::Tri,
            },
            Sm4Decl::HsPartitioning {
                partitioning: HsPartitioning::FractionalEven,
            },
            Sm4Decl::HsOutputTopology {
                topology: HsOutputTopology::TriangleCw,
            },
        ]
    );
    assert_eq!(module.instructions, vec![Sm4Inst::Ret]);
}

#[test]
fn decodes_hull_shader_phase_markers_as_decls() {
    let mut body = Vec::<u32>::new();

    body.push(opcode_token(OPCODE_HS_CONTROL_POINT_PHASE, 1));

    // mov r0, l(0,0,0,0)
    let mut mov0 = vec![opcode_token(OPCODE_MOV, 8)];
    mov0.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov0.extend_from_slice(&imm32_vec4([0, 0, 0, 0]));
    body.extend_from_slice(&mov0);

    body.push(opcode_token(OPCODE_HS_FORK_PHASE, 1));

    // mov r1, l(0,0,0,0)
    let mut mov1 = vec![opcode_token(OPCODE_MOV, 8)];
    mov1.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    mov1.extend_from_slice(&imm32_vec4([0, 0, 0, 0]));
    body.extend_from_slice(&mov1);

    body.push(opcode_token(OPCODE_HS_JOIN_PHASE, 1));

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(3, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");

    let module = decode_program(&program).expect("decode");
    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::HsPhase {
                phase: HullShaderPhase::ControlPoint,
                inst_index: 0,
            },
            Sm4Decl::HsPhase {
                phase: HullShaderPhase::Fork,
                inst_index: 1,
            },
            Sm4Decl::HsPhase {
                phase: HullShaderPhase::Join,
                inst_index: 2,
            },
        ]
    );
    assert_eq!(module.instructions.len(), 3);
    assert!(matches!(&module.instructions[0], Sm4Inst::Mov { .. }));
    assert!(matches!(&module.instructions[1], Sm4Inst::Mov { .. }));
    assert!(matches!(&module.instructions[2], Sm4Inst::Ret));
}

#[test]
fn decodes_sm5_compute_thread_group_and_raw_uav_ops() {
    let mut body = Vec::<u32>::new();

    // dcl_thread_group 8, 8, 1
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_THREAD_GROUP, 4), 8, 8, 1]);

    // ld_raw r0, l(0), t0
    let addr = imm32_scalar(0);
    let mut ld_raw = vec![opcode_token(OPCODE_LD_RAW, (1 + 2 + addr.len() + 2) as u32)];
    ld_raw.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld_raw.extend_from_slice(&addr);
    ld_raw.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ld_raw);

    // store_raw u0.xyzw, l(0), r0
    let uav = uav_operand(0, WriteMask::XYZW);
    let addr = imm32_scalar(0);
    let val = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut store_raw = vec![opcode_token(
        OPCODE_STORE_RAW,
        (1 + uav.len() + addr.len() + val.len()) as u32,
    )];
    store_raw.extend_from_slice(&uav);
    store_raw.extend_from_slice(&addr);
    store_raw.extend_from_slice(&val);
    body.extend_from_slice(&store_raw);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 is compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("decode");
    assert_eq!(module.stage, aero_d3d11::ShaderStage::Compute);

    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::ThreadGroupSize { x: 8, y: 8, z: 1 })));

    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::LdRaw { .. })));
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::StoreRaw { .. })));
}

#[test]
fn decodes_compute_builtin_operand_types() {
    use aero_d3d11::sm4_ir::ComputeBuiltin;

    let mut body = Vec::<u32>::new();

    // mov r0, thread_id (SV_DispatchThreadID)
    let src = reg_src(
        OPERAND_TYPE_INPUT_THREAD_ID,
        &[],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
    mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov.extend_from_slice(&src);
    body.extend_from_slice(&mov);

    // mov r1, thread_group_id (SV_GroupID)
    let src = reg_src(
        OPERAND_TYPE_INPUT_THREAD_GROUP_ID,
        &[],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
    mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    mov.extend_from_slice(&src);
    body.extend_from_slice(&mov);

    // mov r2, thread_id_in_group (SV_GroupThreadID)
    let src = reg_src(
        OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP,
        &[],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
    mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    mov.extend_from_slice(&src);
    body.extend_from_slice(&mov);

    // mov r3, thread_id_in_group_flattened (SV_GroupIndex)
    //
    // Encode this as a scalar select1 operand (matching how scalar operands are represented in
    // SM4/5 token streams).
    let src = vec![operand_token(
        OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP_FLATTENED,
        1,
        OPERAND_SEL_SELECT1,
        0,
        0,
        false,
    )];
    let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
    mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    mov.extend_from_slice(&src);
    body.extend_from_slice(&mov);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::Mov {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            src: SrcOperand {
                kind: SrcKind::ComputeBuiltin(ComputeBuiltin::DispatchThreadId),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }
    );
    assert_eq!(
        module.instructions[1],
        Sm4Inst::Mov {
            dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
            src: SrcOperand {
                kind: SrcKind::ComputeBuiltin(ComputeBuiltin::GroupId),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }
    );
    assert_eq!(
        module.instructions[2],
        Sm4Inst::Mov {
            dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
            src: SrcOperand {
                kind: SrcKind::ComputeBuiltin(ComputeBuiltin::GroupThreadId),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }
    );
    assert_eq!(
        module.instructions[3],
        Sm4Inst::Mov {
            dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
            src: SrcOperand {
                kind: SrcKind::ComputeBuiltin(ComputeBuiltin::GroupIndex),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
        }
    );
    assert!(matches!(&module.instructions[4], Sm4Inst::Ret));
}

#[test]
fn rejects_truncated_sm5_thread_group_decl() {
    // `dcl_thread_group` has a fixed length of 4 DWORDs (opcode + x,y,z). Ensure the decoder
    // rejects token streams that end early instead of panicking.
    let body = [opcode_token(OPCODE_DCL_THREAD_GROUP, 4), 8, 8];

    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");

    let err = decode_program(&program).expect_err("decode should fail");
    assert_eq!(err.at_dword, 2);
    assert!(matches!(
        err.kind,
        aero_d3d11::sm4::decode::Sm4DecodeErrorKind::InstructionOutOfBounds {
            start: 2,
            len: 4,
            available: 5
        }
    ));
}

#[test]
fn rejects_sm5_thread_group_decl_with_too_small_declared_len() {
    // Malformed opcode token that claims the declaration is shorter than its fixed payload.
    // (Still in-bounds per the length field, so this exercises the per-declaration reader.)
    let body = [
        opcode_token(OPCODE_DCL_THREAD_GROUP, 1),
        opcode_token(OPCODE_RET, 1),
    ];

    let tokens = make_sm5_program_tokens(5, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");

    let err = decode_program(&program).expect_err("decode should fail");
    assert_eq!(err.at_dword, 3);
    assert!(matches!(
        err.kind,
        aero_d3d11::sm4::decode::Sm4DecodeErrorKind::UnexpectedEof { .. }
    ));
}

#[test]
fn rejects_windows_sdk_dxbc_token_encoding() {
    // Real DXBC encodes instruction length in bits 24..=30 of the opcode token.
    // Our in-tree SM4 decoder currently expects Aero's legacy encoding (length in bits 11..=23).
    //
    // Use a real DXBC-looking opcode token: mov with len=5, opcode=54 (0x36).
    let body = [0x0500_0036u32];

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");

    let err = decode_program(&program).expect_err("decode should fail");
    assert_eq!(err.at_dword, 2);
    assert!(matches!(
        err.kind,
        Sm4DecodeErrorKind::UnsupportedTokenEncoding { .. }
    ));
}

#[test]
fn rejects_windows_sdk_dxbc_token_encoding_when_length_bits_overflow_internal_len() {
    // Use a real DXBC-looking opcode token for `if_nz`:
    // - opcode = 31
    // - length (bits 24..30) = 3 DWORDs
    // - test boolean (bit 18) = nonzero
    //
    // In Aero's legacy encoding this would look like an absurd instruction length, so we should
    // surface `UnsupportedTokenEncoding` rather than a generic out-of-bounds error.
    let opcode_token = 0x0304_001fu32;

    // Pad the token stream so the official length field is in-bounds.
    let body = [opcode_token, 0, 0];

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");

    let err = decode_program(&program).expect_err("decode should fail");
    assert_eq!(err.at_dword, 2);
    assert!(matches!(
        err.kind,
        Sm4DecodeErrorKind::UnsupportedTokenEncoding { .. }
    ));
}

#[test]
fn decodes_atomic_add_via_structural_fallback() {
    // Pick an opcode that is not otherwise recognized by the decoder and rely on the structural
    // decoding path.
    // Note: this value must not collide with any opcode we explicitly support (e.g. integer ops).
    const OPCODE_UNKNOWN_ATOMIC_IADD: u32 = 0x7b;

    let mut body = Vec::<u32>::new();

    // atomic_iadd r0.x, u0, l(0), l(1)
    let dst0 = reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::X);
    let uav0 = uav_operand(0, WriteMask::XYZW);
    let addr = imm32_scalar(0);
    let value = imm32_scalar(1);
    let mut inst = vec![opcode_token(
        OPCODE_UNKNOWN_ATOMIC_IADD,
        (1 + dst0.len() + uav0.len() + addr.len() + value.len()) as u32,
    )];
    inst.extend_from_slice(&dst0);
    inst.extend_from_slice(&uav0);
    inst.extend_from_slice(&addr);
    inst.extend_from_slice(&value);
    body.extend_from_slice(&inst);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::AtomicAdd {
            dst: Some(dst(RegFile::Temp, 0, WriteMask::X)),
            uav: UavRef { slot: 0 },
            addr: SrcOperand {
                kind: SrcKind::ImmediateF32([0, 0, 0, 0]),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            value: SrcOperand {
                kind: SrcKind::ImmediateF32([1, 1, 1, 1]),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
        }
    );
}

#[test]
fn decodes_ld_raw() {
    let mut body = Vec::<u32>::new();

    // ld_raw r0, r1.x, t0
    let addr = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let mut ld_raw = vec![opcode_token(OPCODE_LD_RAW, (1 + 2 + addr.len() + 2) as u32)];
    ld_raw.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld_raw.extend_from_slice(&addr);
    ld_raw.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ld_raw);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::LdRaw {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            addr: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            buffer: BufferRef { slot: 0 },
        }
    );
}

#[test]
fn decodes_ld_raw_from_uav() {
    let mut body = Vec::<u32>::new();

    // ld_raw r0, r1.x, u0
    let addr = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let uav = uav_operand(0, WriteMask::XYZW);
    let mut ld_raw = vec![opcode_token(
        OPCODE_LD_RAW,
        (1 + 2 + addr.len() + uav.len()) as u32,
    )];
    ld_raw.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld_raw.extend_from_slice(&addr);
    ld_raw.extend_from_slice(&uav);
    body.extend_from_slice(&ld_raw);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::LdUavRaw {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            addr: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            uav: UavRef { slot: 0 },
        }
    );
}

#[test]
fn decodes_store_raw_with_mask() {
    let mut body = Vec::<u32>::new();

    // store_raw u0.xy, r0.x, r1
    let addr = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let value = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let uav = uav_operand(0, WriteMask(0b0011));
    let mut store_raw = vec![opcode_token(
        OPCODE_STORE_RAW,
        (1 + uav.len() + addr.len() + value.len()) as u32,
    )];
    store_raw.extend_from_slice(&uav);
    store_raw.extend_from_slice(&addr);
    store_raw.extend_from_slice(&value);
    body.extend_from_slice(&store_raw);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::StoreRaw {
            uav: UavRef { slot: 0 },
            addr: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 0,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            value: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1,
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            mask: WriteMask(0b0011),
        }
    );
}

#[test]
fn decodes_store_raw_via_structural_fallback() {
    let mut body = Vec::<u32>::new();

    // Some SM5 producers appear to use different opcode IDs for buffer UAV stores. The decoder
    // includes a structural fallback path so we can still recognize `store_raw` forms.
    //
    // Encode: store_raw u0.xy, r0.x, r1
    let addr = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let value = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let uav = uav_operand(0, WriteMask(0b0011));

    let unknown_opcode = 0x7b;
    let mut store_raw = vec![opcode_token(
        unknown_opcode,
        (1 + uav.len() + addr.len() + value.len()) as u32,
    )];
    store_raw.extend_from_slice(&uav);
    store_raw.extend_from_slice(&addr);
    store_raw.extend_from_slice(&value);
    body.extend_from_slice(&store_raw);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(
        matches!(&module.instructions[0], Sm4Inst::StoreRaw { .. }),
        "expected structural fallback to decode StoreRaw, got {:?}",
        module.instructions[0]
    );
}

#[test]
fn refines_unknown_uav_store_to_raw_when_decl_is_buffer() {
    let mut body = Vec::<u32>::new();

    // dcl_uav_raw u0
    let u0_decl = reg_src(
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_UAV_RAW, (1 + u0_decl.len()) as u32)]);
    body.extend_from_slice(&u0_decl);

    // Encode an unknown store opcode that matches the typed-store structural decoder
    // (`u#, coord, value`) but uses a buffer UAV slot. Post-processing should refine it into
    // `StoreRaw`.
    let addr = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let value = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let uav = uav_operand(0, WriteMask::XYZW);

    let unknown_opcode = 0x7c;
    let mut store = vec![opcode_token(
        unknown_opcode,
        (1 + uav.len() + addr.len() + value.len()) as u32,
    )];
    store.extend_from_slice(&uav);
    store.extend_from_slice(&addr);
    store.extend_from_slice(&value);
    body.extend_from_slice(&store);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(
        matches!(&module.instructions[0], Sm4Inst::StoreRaw { .. }),
        "expected decl-based refinement to yield StoreRaw, got {:?}",
        module.instructions[0]
    );
}

#[test]
fn refines_unknown_uav_store_to_typed_when_decl_is_typed() {
    let mut body = Vec::<u32>::new();

    // Typed UAV declaration: use an opcode ID that the decoder does not have a constant for and
    // rely on the declaration decoder's typed UAV path.
    //
    // dcl_uav_typed u0, dim=2, format=28 (DXGI_FORMAT_R8G8B8A8_UNORM)
    let u0_decl = reg_src(
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let unknown_decl_opcode = 0x209;
    let mut dcl_uav_typed = vec![opcode_token(
        unknown_decl_opcode,
        (1 + u0_decl.len() + 2) as u32,
    )];
    dcl_uav_typed.extend_from_slice(&u0_decl);
    dcl_uav_typed.push(2); // dim = 2D
    dcl_uav_typed.push(28); // DXGI_FORMAT_R8G8B8A8_UNORM
    body.extend_from_slice(&dcl_uav_typed);

    // Unknown store opcode with scalar coord; structural decoding will initially treat it like a
    // raw buffer store, but post-processing should refine it into a typed UAV store because `u0`
    // is declared as typed.
    let coord = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let value = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let uav = uav_operand(0, WriteMask::XYZW);

    let unknown_store_opcode = 0x7d;
    let mut store = vec![opcode_token(
        unknown_store_opcode,
        (1 + uav.len() + coord.len() + value.len()) as u32,
    )];
    store.extend_from_slice(&uav);
    store.extend_from_slice(&coord);
    store.extend_from_slice(&value);
    body.extend_from_slice(&store);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::UavTyped2D {
                slot: 0,
                format: 28
            }
        )),
        "expected typed UAV declaration to be decoded"
    );
    assert!(
        matches!(&module.instructions[0], Sm4Inst::StoreUavTyped { .. }),
        "expected decl-based refinement to yield StoreUavTyped, got {:?}",
        module.instructions[0]
    );
}

#[test]
fn decodes_buffer_srv_and_uav_declarations() {
    let mut body = Vec::<u32>::new();

    // dcl_resource_raw t0
    let t0 = reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_RESOURCE_RAW, (1 + t0.len()) as u32)]);
    body.extend_from_slice(&t0);

    // dcl_resource_structured t1, 16
    let t1 = reg_src(
        OPERAND_TYPE_RESOURCE,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(
        OPCODE_DCL_RESOURCE_STRUCTURED,
        (1 + t1.len() + 1) as u32,
    )]);
    body.extend_from_slice(&t1);
    body.push(16);

    // dcl_uav_raw u0
    let u0 = reg_src(
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(OPCODE_DCL_UAV_RAW, (1 + u0.len()) as u32)]);
    body.extend_from_slice(&u0);

    // dcl_uav_structured u1, 32
    let u1 = reg_src(
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    body.extend_from_slice(&[opcode_token(
        OPCODE_DCL_UAV_STRUCTURED,
        (1 + u1.len() + 1) as u32,
    )]);
    body.extend_from_slice(&u1);
    body.push(32);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::ResourceBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
            Sm4Decl::ResourceBuffer {
                slot: 1,
                stride: 16,
                kind: BufferKind::Structured,
            },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
            Sm4Decl::UavBuffer {
                slot: 1,
                stride: 32,
                kind: BufferKind::Structured,
            },
        ]
    );
}

#[test]
fn decodes_ld_structured() {
    let mut body = Vec::<u32>::new();

    // ld_structured r0, r1.x, r2.x, t0
    let index = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let offset = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let mut ld_structured = vec![opcode_token(
        OPCODE_LD_STRUCTURED,
        (1 + 2 + index.len() + offset.len() + 2) as u32,
    )];
    ld_structured.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld_structured.extend_from_slice(&index);
    ld_structured.extend_from_slice(&offset);
    ld_structured.extend_from_slice(&reg_src(
        OPERAND_TYPE_RESOURCE,
        &[0],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&ld_structured);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::LdStructured {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            index: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            offset: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 2,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            buffer: BufferRef { slot: 0 },
        }
    );
}

#[test]
fn decodes_ld_structured_from_uav() {
    let mut body = Vec::<u32>::new();

    // ld_structured r0, r1.x, r2.x, u0
    let index = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let offset = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let uav = uav_operand(0, WriteMask::XYZW);

    let mut ld_structured = vec![opcode_token(
        OPCODE_LD_STRUCTURED,
        (1 + 2 + index.len() + offset.len() + uav.len()) as u32,
    )];
    ld_structured.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld_structured.extend_from_slice(&index);
    ld_structured.extend_from_slice(&offset);
    ld_structured.extend_from_slice(&uav);
    body.extend_from_slice(&ld_structured);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::LdStructuredUav {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            index: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            offset: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 2,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            uav: UavRef { slot: 0 },
        }
    );
}

#[test]
fn decodes_ld_structured_uav_via_structural_fallback() {
    let mut body = Vec::<u32>::new();

    // Some SM5 producers may use a distinct opcode for `ld_uav_structured` instead of reusing the
    // `ld_structured` opcode with a `u#` operand. The decoder includes a structural fallback path
    // so we can still recognize the instruction even when the opcode ID differs.
    //
    // Encode: ld_uav_structured r0, r1.x, r2.x, u0
    let index = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let offset = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let uav = uav_operand(0, WriteMask::XYZW);

    // Use an opcode ID that the decoder does not have a direct match arm for.
    let unknown_opcode = 0x7a;
    let mut ld_structured = vec![opcode_token(
        unknown_opcode,
        (1 + 2 + index.len() + offset.len() + uav.len()) as u32,
    )];
    ld_structured.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ld_structured.extend_from_slice(&index);
    ld_structured.extend_from_slice(&offset);
    ld_structured.extend_from_slice(&uav);
    body.extend_from_slice(&ld_structured);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(
        matches!(&module.instructions[0], Sm4Inst::LdStructuredUav { .. }),
        "expected structural fallback to decode LdStructuredUav, got {:?}",
        module.instructions[0]
    );
}

#[test]
fn decodes_float_cmp_opcodes() {
    let mut body = Vec::<u32>::new();

    let ops = [
        (OPCODE_EQ, CmpOp::Eq),
        (OPCODE_NE, CmpOp::Ne),
        (OPCODE_LT, CmpOp::Lt),
        (OPCODE_GE, CmpOp::Ge),
    ];

    let mut expected = Vec::<Sm4Inst>::new();
    for (dst_idx, (opcode, op)) in ops.iter().copied().enumerate() {
        let dst_idx = dst_idx as u32;

        // <cmp> rN, r1, r2
        let mut inst = vec![opcode_token(opcode, 1 + 2 + 2 + 2)];
        inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, dst_idx, WriteMask::XYZW));
        inst.extend_from_slice(&reg_src(
            OPERAND_TYPE_TEMP,
            &[1],
            Swizzle::XYZW,
            OperandModifier::None,
        ));
        inst.extend_from_slice(&reg_src(
            OPERAND_TYPE_TEMP,
            &[2],
            Swizzle::XYZW,
            OperandModifier::None,
        ));
        body.extend_from_slice(&inst);

        expected.push(Sm4Inst::Cmp {
            op,
            ty: CmpType::F32,
            dst: dst(RegFile::Temp, dst_idx, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 1),
            b: src_reg(RegFile::Temp, 2),
        });
    }

    body.push(opcode_token(OPCODE_RET, 1));
    expected.push(Sm4Inst::Ret);

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.instructions, expected);
}

#[test]
fn decodes_float_cmp_ignores_saturate_modifier() {
    let mut body = Vec::<u32>::new();

    // lt_sat r0, r1, r2
    let len_without_ext = 1u32 + 2 + 2 + 2;
    let mut lt = opcode_token_with_sat(OPCODE_LT, len_without_ext);
    lt.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    lt.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    lt.extend_from_slice(&reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    ));
    body.extend_from_slice(&lt);
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    // Saturate is ignored for compares (predicate mask bits must remain unmodified).
    assert_eq!(
        module.instructions[0],
        Sm4Inst::Cmp {
            dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
            a: src_reg(RegFile::Temp, 1),
            b: src_reg(RegFile::Temp, 2),
            op: aero_d3d11::CmpOp::Lt,
            ty: aero_d3d11::CmpType::F32,
        }
    );
}

#[test]
fn decodes_store_structured_with_mask() {
    let mut body = Vec::<u32>::new();

    // store_structured u0.xy, r0.x, r1.x, r2
    let uav = uav_operand(0, WriteMask(0b0011));
    let index = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let offset = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let value = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    );
    let mut store_structured = vec![opcode_token(
        OPCODE_STORE_STRUCTURED,
        (1 + uav.len() + index.len() + offset.len() + value.len()) as u32,
    )];
    store_structured.extend_from_slice(&uav);
    store_structured.extend_from_slice(&index);
    store_structured.extend_from_slice(&offset);
    store_structured.extend_from_slice(&value);
    body.extend_from_slice(&store_structured);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions[0],
        Sm4Inst::StoreStructured {
            uav: UavRef { slot: 0 },
            index: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 0,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            offset: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 1,
                }),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            value: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 2,
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            mask: WriteMask(0b0011),
        }
    );
}

#[test]
fn decodes_store_structured_via_structural_fallback() {
    let mut body = Vec::<u32>::new();

    // Some SM5 producers appear to use different opcode IDs for `store_structured`. The decoder
    // includes a structural fallback path so we can still recognize the instruction shape.
    //
    // Encode: store_structured u0.xy, r0.x, r1.x, r2
    let uav = uav_operand(0, WriteMask(0b0011));
    let index = reg_src(
        OPERAND_TYPE_TEMP,
        &[0],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let offset = reg_src(
        OPERAND_TYPE_TEMP,
        &[1],
        Swizzle::XXXX,
        OperandModifier::None,
    );
    let value = reg_src(
        OPERAND_TYPE_TEMP,
        &[2],
        Swizzle::XYZW,
        OperandModifier::None,
    );

    let unknown_opcode = 0x7e;
    let mut store_structured = vec![opcode_token(
        unknown_opcode,
        (1 + uav.len() + index.len() + offset.len() + value.len()) as u32,
    )];
    store_structured.extend_from_slice(&uav);
    store_structured.extend_from_slice(&index);
    store_structured.extend_from_slice(&offset);
    store_structured.extend_from_slice(&value);
    body.extend_from_slice(&store_structured);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert!(
        matches!(&module.instructions[0], Sm4Inst::StoreStructured { .. }),
        "expected structural fallback to decode StoreStructured, got {:?}",
        module.instructions[0]
    );
}

#[test]
fn decodes_emit_stream_and_cut_stream_with_stream_index() {
    let mut body = Vec::<u32>::new();

    // emit_stream(2)
    body.push(opcode_token(OPCODE_EMIT_STREAM, 3));
    body.extend_from_slice(&imm32_scalar(2));
    // cut_stream(3)
    body.push(opcode_token(OPCODE_CUT_STREAM, 3));
    body.extend_from_slice(&imm32_scalar(3));
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 2 is geometry shader.
    let tokens = make_sm5_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(
        module.instructions,
        vec![
            Sm4Inst::Emit { stream: 2 },
            Sm4Inst::Cut { stream: 3 },
            Sm4Inst::Ret
        ]
    );
}
