use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module,
    Sm4Program, SrcKind, Swizzle, WriteMask,
};

fn make_sm4_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout:
    // type in bits 16.., major in bits 4..7, minor in bits 0..3.
    let version = ((stage_type as u32) << 16) | (4u32 << 4);
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

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

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle) -> Vec<u32> {
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
        false,
    );
    let mut out = Vec::new();
    out.push(token);
    out.extend_from_slice(indices);
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

#[test]
fn decodes_geometry_shader_decls_and_emit_cut() {
    // These enum values are not interpreted by the decoder today; they are carried through so
    // later stages can decide whether to emulate geometry shaders.
    const PRIM_TRIANGLE: u32 = 0x4;
    const TOPO_TRIANGLE_STRIP: u32 = 0x5;
    const MAX_VERTS: u32 = 3;

    // Use dummy opcodes for `dcl_input`/`dcl_output` in the test stream. The decoder doesn't
    // currently distinguish declaration kinds by opcode; it looks at the operand type.
    const DCL_DUMMY: u32 = 0x300;

    let mut body = Vec::<u32>::new();

    // Geometry shader metadata declarations (no operand tokens).
    body.extend_from_slice(&[
        opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2),
        PRIM_TRIANGLE,
        opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2),
        TOPO_TRIANGLE_STRIP,
        opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2),
        MAX_VERTS,
    ]);

    // Basic IO declarations.
    // dcl_input v0.xyzw
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::XYZW));
    // dcl_output o0.xyzw
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 1, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    // dcl_output o1.xyzw
    body.extend_from_slice(&[opcode_token(DCL_DUMMY + 2, 3)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));

    // mov r0, v0[1]
    let mut mov_in = vec![opcode_token(OPCODE_MOV, 1 + 2 + 3)];
    mov_in.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov_in.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0, 1], Swizzle::XYZW));
    body.extend_from_slice(&mov_in);

    // mov o0, r0
    let mut mov_o0 = vec![opcode_token(OPCODE_MOV, 1 + 2 + 2)];
    mov_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    mov_o0.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
    body.extend_from_slice(&mov_o0);

    // mov o1, r0
    let mut mov_o1 = vec![opcode_token(OPCODE_MOV, 1 + 2 + 2)];
    mov_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    mov_o1.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));
    body.extend_from_slice(&mov_o1);

    // emit
    body.push(opcode_token(OPCODE_EMIT, 1));
    // cut
    body.push(opcode_token(OPCODE_CUT, 1));
    // ret
    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm4_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = aero_d3d11::sm4::decode_program(&program).expect("decode");

    assert_eq!(
        module,
        Sm4Module {
            stage: ShaderStage::Geometry,
            model: ShaderModel { major: 4, minor: 0 },
            decls: vec![
                Sm4Decl::GsInputPrimitive {
                    primitive: PRIM_TRIANGLE
                },
                Sm4Decl::GsOutputTopology {
                    topology: TOPO_TRIANGLE_STRIP
                },
                Sm4Decl::GsMaxOutputVertexCount { max: MAX_VERTS },
                Sm4Decl::Input {
                    reg: 0,
                    mask: WriteMask::XYZW
                },
                Sm4Decl::Output {
                    reg: 0,
                    mask: WriteMask::XYZW
                },
                Sm4Decl::Output {
                    reg: 1,
                    mask: WriteMask::XYZW
                },
            ],
            instructions: vec![
                Sm4Inst::Mov {
                    dst: aero_d3d11::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: aero_d3d11::SrcOperand {
                        kind: SrcKind::GsInput { reg: 0, vertex: 1 },
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Mov {
                    dst: aero_d3d11::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 0
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: aero_d3d11::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 0
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Mov {
                    dst: aero_d3d11::DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Output,
                            index: 1
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    src: aero_d3d11::SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 0
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                },
                Sm4Inst::Emit { stream: 0 },
                Sm4Inst::Cut { stream: 0 },
                Sm4Inst::Ret,
            ],
        }
    );
}

#[test]
fn decodes_geometry_shader_emit_cut_stream_variants() {
    // emit_stream l(2)
    let mut body = Vec::<u32>::new();

    let stream = 2u32;
    let stream_op = imm32_scalar(stream);

    body.push(opcode_token(
        OPCODE_EMIT_STREAM,
        (1 + stream_op.len()) as u32,
    ));
    body.extend_from_slice(&stream_op);

    // cut_stream l(2)
    body.push(opcode_token(
        OPCODE_CUT_STREAM,
        (1 + stream_op.len()) as u32,
    ));
    body.extend_from_slice(&stream_op);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm4_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = aero_d3d11::sm4::decode_program(&program).expect("decode");

    assert_eq!(module.stage, ShaderStage::Geometry);
    assert_eq!(
        module.instructions,
        vec![
            Sm4Inst::Emit { stream },
            Sm4Inst::Cut { stream },
            Sm4Inst::Ret
        ]
    );
}

#[test]
fn gs_opcode_constants_match_d3d10_tokenized_format() {
    // Keep these in sync with `d3d10tokenizedprogramformat.h` (`D3D10_SB_OPCODE_TYPE`).
    assert_eq!(OPCODE_DCL_GS_INPUT_PRIMITIVE, 0x10c);
    assert_eq!(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 0x10d);
    assert_eq!(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 0x10e);
    assert_eq!(OPCODE_EMIT_STREAM, 0x41);
    assert_eq!(OPCODE_CUT_STREAM, 0x42);
    assert_eq!(OPCODE_EMIT, 0x43);
    assert_eq!(OPCODE_CUT, 0x44);
    assert_eq!(OPCODE_EMITTHENCUT, 0x3f);
    assert_eq!(OPCODE_EMITTHENCUT_STREAM, 0x40);
}

#[test]
fn decodes_emitthen_cut_variants() {
    let mut body = Vec::<u32>::new();
    body.push(opcode_token(OPCODE_EMITTHENCUT, 1));

    // emitthen_cut_stream 2
    let stream_op = imm32_scalar(2);
    let mut emit_then_cut_stream = vec![opcode_token(
        OPCODE_EMITTHENCUT_STREAM,
        1 + stream_op.len() as u32,
    )];
    emit_then_cut_stream.extend_from_slice(&stream_op);
    body.extend_from_slice(&emit_then_cut_stream);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm4_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = aero_d3d11::sm4::decode_program(&program).expect("decode");

    assert_eq!(module.stage, ShaderStage::Geometry);
    assert!(matches!(
        module.instructions.as_slice(),
        [
            Sm4Inst::EmitThenCut { stream: 0 },
            Sm4Inst::EmitThenCut { stream: 2 },
            Sm4Inst::Ret
        ]
    ));
}

#[test]
fn decodes_stream_ops_without_operand_default_to_stream0() {
    // Some real-world SM4 blobs omit the immediate operand for stream 0 on the `_stream`
    // instruction forms. Ensure the decoder accepts that encoding and defaults to stream 0.
    let body = [
        opcode_token(OPCODE_EMIT_STREAM, 1),
        opcode_token(OPCODE_CUT_STREAM, 1),
        opcode_token(OPCODE_EMITTHENCUT_STREAM, 1),
        opcode_token(OPCODE_RET, 1),
    ];

    let tokens = make_sm4_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = aero_d3d11::sm4::decode_program(&program).expect("decode");

    assert_eq!(module.stage, ShaderStage::Geometry);
    assert!(matches!(
        module.instructions.as_slice(),
        [
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Cut { stream: 0 },
            Sm4Inst::EmitThenCut { stream: 0 },
            Sm4Inst::Ret
        ]
    ));
}

#[test]
fn decodes_geometry_shader_instance_count_decl_sm5() {
    const INSTANCE_COUNT: u32 = 4;
    // `SV_GSInstanceID` D3D name token (see `shader_translate.rs`).
    const D3D_NAME_GS_INSTANCE_ID: u32 = 11;

    // Use dummy opcode for `dcl_input_siv`; the decoder keys off operand type and the trailing
    // sys-value token rather than the declaration opcode.
    const DCL_DUMMY: u32 = 0x320;

    let mut body = Vec::<u32>::new();

    // dcl_gsinstancecount INSTANCE_COUNT
    body.extend_from_slice(&[
        opcode_token(OPCODE_DCL_GS_INSTANCE_COUNT, 2),
        INSTANCE_COUNT,
    ]);

    // dcl_input_siv v0.x, gsinstanceid
    body.extend_from_slice(&[opcode_token(DCL_DUMMY, 4)]);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::X));
    body.push(D3D_NAME_GS_INSTANCE_ID);

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(2, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = aero_d3d11::sm4::decode_program(&program).expect("decode");

    assert_eq!(module.stage, ShaderStage::Geometry);
    assert_eq!(module.model, ShaderModel { major: 5, minor: 0 });
    assert_eq!(
        module.decls,
        vec![
            Sm4Decl::GsInstanceCount {
                count: INSTANCE_COUNT
            },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::X,
                sys_value: D3D_NAME_GS_INSTANCE_ID
            }
        ]
    );
    assert_eq!(module.instructions, vec![Sm4Inst::Ret]);
}
