use aero_d3d11::binding_model::{
    BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, BIND_GROUP_INTERNAL_EMULATION,
};
use aero_d3d11::runtime::gs_translate::{
    translate_gs_module_to_wgsl_compute_prepass,
    translate_gs_module_to_wgsl_compute_prepass_packed, GsTranslateError,
};
use aero_d3d11::sm4::decode_program;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    BufferKind, BufferRef, DstOperand, GsInputPrimitive, GsOutputTopology, OperandModifier,
    PredicateDstOperand, PredicateOperand, PredicateRef, RegFile, RegisterRef, SamplerRef,
    ShaderModel, ShaderStage, Sm4CmpOp, Sm4Decl, Sm4Inst, Sm4Module, Sm4Program, Sm4TestBool,
    SrcKind, SrcOperand, Swizzle, TextureRef, WriteMask,
};

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
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
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1),
        idx,
    ]
}

fn reg_src(ty: u32, idx: u32) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_SWIZZLE, swizzle_bits(Swizzle::XYZW.0), 1),
        idx,
    ]
}

fn reg_src_swizzle_modifier(ty: u32, idx: u32, swz: [u8; 4], modifier: u32) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_SWIZZLE, swizzle_bits(swz), 1) | OPERAND_EXTENDED_BIT,
        modifier << 6,
        idx,
    ]
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(Swizzle::XYZW.0),
        0,
    ));
    out.extend_from_slice(&values);
    out
}

fn assert_wgsl_validates(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("generated WGSL failed to validate");
}

fn base_gs_tokens() -> Vec<u32> {
    // Nominal gs_4_0 version token (decoder uses program.stage/model, but the header must be
    // well-formed).
    let version_token = 0x0003_0040u32;

    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations required by `gs_translate`.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // D3D10_SB_PRIMITIVE_TRIANGLE
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // Declare outputs so the decoder produces `Sm4Decl::Output` entries (not strictly required by
    // the GS prepass translator, but keeps the token streams realistic).
    // dcl_output o0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    // dcl_output o1.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    tokens
}

fn wgsl_from_tokens(mut tokens: Vec<u32>) -> String {
    tokens[1] = tokens.len() as u32;
    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };
    let module = decode_program(&program).expect("decode");
    translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate")
}

#[test]
fn sm4_gs_packed_varying_o2_translates_to_expanded_vertex_location2() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(5),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 2,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Ret,
        ],
    };

    let wgsl =
        translate_gs_module_to_wgsl_compute_prepass_packed(&module, &[2]).expect("translate");
    assert!(
        wgsl.contains("out_vertices.data[vtx_idx].v0 = o2;"),
        "expected packed output register o2 to be written to ExpandedVertex.v0:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_packed_varying_missing_output_register_defaults_to_zero() {
    // The shader never writes `o5`, but the packed layout requests it. The translator should still
    // declare `o5` (zero-initialized) and pack it into the expanded-vertex buffer.
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(5),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Ret,
        ],
    };

    let wgsl =
        translate_gs_module_to_wgsl_compute_prepass_packed(&module, &[5]).expect("translate");
    assert!(
        wgsl.contains("var o5: vec4<f32> = vec4<f32>(0.0);"),
        "expected output register o5 to be declared/zero-initialized:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_vertices.data[vtx_idx].v0 = o5;"),
        "expected packed output register o5 to be written to ExpandedVertex.v0:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_packed_rejects_location_0() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(5),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
        ],
        instructions: vec![Sm4Inst::Ret],
    };

    let err = translate_gs_module_to_wgsl_compute_prepass_packed(&module, &[0])
        .expect_err("expected location 0 to be rejected (reserved for position)");
    assert_eq!(err, GsTranslateError::InvalidVaryingLocation { loc: 0 });
}

#[test]
fn sm4_gs_sample_emits_group3_texture_and_sampler_decls() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::Point(1),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::ResourceTexture2D { slot: 0 },
            Sm4Decl::Sampler { slot: 0 },
        ],
        instructions: vec![
            // Position (o0) must be initialized before emit.
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0, 0, 0, 0x3f800000]), // (0,0,0,1)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            // Sample into o1.
            Sm4Inst::Sample {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 1,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                coord: SrcOperand {
                    kind: SrcKind::ImmediateF32([0x3f000000, 0x3e800000, 0, 0]), // (0.5, 0.25, 0, 0)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef { slot: 0 },
            },
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");
    assert!(
        wgsl.contains(&format!(
            "@group({BIND_GROUP_INTERNAL_EMULATION}) @binding({}) var t0: texture_2d<f32>;",
            BINDING_BASE_TEXTURE
        )),
        "expected group(3) texture declaration in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains(&format!(
            "@group({BIND_GROUP_INTERNAL_EMULATION}) @binding({}) var s0: sampler;",
            BINDING_BASE_SAMPLER
        )),
        "expected group(3) sampler declaration in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("textureSampleLevel(t0, s0"),
        "expected sample to lower to textureSampleLevel in WGSL:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_ld_raw_emits_group3_srv_buffer_decl() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::Point(1),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::ResourceBuffer {
                slot: 1,
                stride: 0,
                kind: BufferKind::Raw,
            },
        ],
        instructions: vec![
            Sm4Inst::LdRaw {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                addr: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                buffer: BufferRef { slot: 1 },
            },
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");
    assert!(
        wgsl.contains("struct AeroStorageBufferU32 { data: array<u32> };"),
        "expected AeroStorageBufferU32 wrapper struct in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains(&format!(
            "@group({BIND_GROUP_INTERNAL_EMULATION}) @binding({}) var<storage, read> t1: AeroStorageBufferU32;",
            BINDING_BASE_TEXTURE + 1
        )),
        "expected group(3) SRV buffer declaration in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("t1.data[ld_raw_base"),
        "expected ld_raw lowering to index t1.data:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_resinfo_emits_texture_dimensions_and_num_levels() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::Point(1),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::ResourceTexture2D { slot: 0 },
        ],
        instructions: vec![
            Sm4Inst::ResInfo {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                mip_level: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                texture: TextureRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");
    assert!(
        wgsl.contains(&format!(
            "@group({BIND_GROUP_INTERNAL_EMULATION}) @binding({}) var t0: texture_2d<f32>;",
            BINDING_BASE_TEXTURE
        )),
        "expected group(3) texture declaration in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("textureDimensions(t0, i32("),
        "expected resinfo lowering to query textureDimensions:\n{wgsl}"
    );
    assert!(
        wgsl.contains("textureNumLevels(t0)"),
        "expected resinfo lowering to query textureNumLevels:\n{wgsl}"
    );
    assert!(
        wgsl.contains("bitcast<vec4<f32>>(vec4<u32>"),
        "expected resinfo result to be packed as raw u32 bits:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_bufinfo_raw_emits_array_length_in_bytes() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::Point(1),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::ResourceBuffer {
                slot: 1,
                stride: 0,
                kind: BufferKind::Raw,
            },
        ],
        instructions: vec![
            Sm4Inst::BufInfoRaw {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                buffer: BufferRef { slot: 1 },
            },
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");
    assert!(
        wgsl.contains("struct AeroStorageBufferU32 { data: array<u32> };"),
        "expected AeroStorageBufferU32 wrapper struct in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains(&format!(
            "@group({BIND_GROUP_INTERNAL_EMULATION}) @binding({}) var<storage, read> t1: AeroStorageBufferU32;",
            BINDING_BASE_TEXTURE + 1
        )),
        "expected group(3) SRV buffer declaration in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("arrayLength(&t1.data)"),
        "expected bufinfo lowering to use arrayLength on the buffer:\n{wgsl}"
    );
    assert!(
        wgsl.contains("* 4u"),
        "expected bufinfo lowering to convert dword count to bytes:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_bufinfo_structured_emits_elem_count_and_stride() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::Point(1),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::ResourceBuffer {
                slot: 2,
                stride: 16,
                kind: BufferKind::Structured,
            },
        ],
        instructions: vec![
            Sm4Inst::BufInfoStructured {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                buffer: BufferRef { slot: 2 },
                stride_bytes: 16,
            },
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");
    assert!(
        wgsl.contains("arrayLength(&t2.data)"),
        "expected structured bufinfo lowering to use arrayLength on the buffer:\n{wgsl}"
    );
    assert!(
        wgsl.contains("16u"),
        "expected structured bufinfo lowering to bake in the declared stride:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_emit_cut_translates_to_wgsl_compute_prepass() {
    // Build a minimal gs_4_0 token stream with:
    // - dcl_inputprimitive triangle
    // - dcl_outputtopology triangle_strip
    // - dcl_maxvertexcount 3
    // - mov o0, v0[0]; mov o1, l(1,0,0,1); emit
    // - mov o0, v0[1]; add o0, o0, l(0,0,0,0); emit
    // - mov o0, v0[2]; emit
    // - cut; ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)

    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // triangle (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // triangle_strip (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(3);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_INPUT, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o#.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // mov o1.xyzw, l(1,0,0,1)
    tokens.push(opcode_token(OPCODE_MOV, 8));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0x3f800000); // 1.0
    tokens.push(0);
    tokens.push(0);
    tokens.push(0x3f800000); // 1.0

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // add o0.xyzw, o0.xyzw, l(0,0,0,0)
    tokens.push(opcode_token(OPCODE_ADD, 10));
    tokens.push(0x10F022); // o0.xyzw (dst)
    tokens.push(0);
    tokens.push(0x10F022); // o0.xyzw (src0)
    tokens.push(0);
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0);
    tokens.push(0);
    tokens.push(0);
    tokens.push(0);

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[2].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[2].xyzw
    tokens.push(0); // reg
    tokens.push(2); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // cut
    tokens.push(opcode_token(OPCODE_CUT, 1));

    // ret
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("fn gs_emit"),
        "expected generated WGSL to contain gs_emit helper function"
    );
    assert!(
        wgsl.contains("fn gs_cut"),
        "expected generated WGSL to contain gs_cut helper function"
    );
    assert!(
        wgsl.contains("gs_emit(o0, o1"),
        "expected generated WGSL to call gs_emit:\n{wgsl}"
    );
    assert!(
        wgsl.contains("gs_cut(&strip_len)"),
        "expected generated WGSL to call gs_cut"
    );
    assert!(
        wgsl.contains("v0: vec4<f32>"),
        "expected default ExpandedVertex layout to include a packed v0 varying slot:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_vertices.data[vtx_idx].v0 = o1;"),
        "expected o1 to map to v0 in expanded vertex output:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_float_arithmetic_ops_translate_to_wgsl_compute_prepass() {
    // Ensure the GS prepass translator supports a basic set of arithmetic ops that appear in
    // real-world geometry shaders (mul/mad/dp3/dp4/min/max).
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // triangle (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(5); // triangle_strip
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // dcl_output o0.xyzw / o1.xyzw (opcode values are irrelevant; decoder treats opcode>=0x100 as decl).
    const DCL_DUMMY: u32 = 0x100;
    tokens.push(opcode_token(DCL_DUMMY, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));

    // mov o0.xyzw, l(1, 2, 3, 4)
    let mut mov_o0 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    mov_o0.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
    ]));
    mov_o0[0] = opcode_token(OPCODE_MOV, mov_o0.len() as u32);
    tokens.extend_from_slice(&mov_o0);

    // mov o1.xyzw, l(4, 3, 2, 1)
    let mut mov_o1 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    mov_o1.extend_from_slice(&imm32_vec4([
        4.0f32.to_bits(),
        3.0f32.to_bits(),
        2.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    mov_o1[0] = opcode_token(OPCODE_MOV, mov_o1.len() as u32);
    tokens.extend_from_slice(&mov_o1);

    // mul o0.xyzw, o0.xyzw, l(2, 2, 2, 2)
    let mut mul_o0 = vec![opcode_token(OPCODE_MUL, 0)];
    mul_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    mul_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    mul_o0.extend_from_slice(&imm32_vec4([2.0f32.to_bits(); 4]));
    mul_o0[0] = opcode_token(OPCODE_MUL, mul_o0.len() as u32);
    tokens.extend_from_slice(&mul_o0);

    // mad o1.xyzw, o0.xyzw, l(0.5, 0.5, 0.5, 0.5), o1.xyzw
    let mut mad_o1 = vec![opcode_token(OPCODE_MAD, 0)];
    mad_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    mad_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    mad_o1.extend_from_slice(&imm32_vec4([0.5f32.to_bits(); 4]));
    mad_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    mad_o1[0] = opcode_token(OPCODE_MAD, mad_o1.len() as u32);
    tokens.extend_from_slice(&mad_o1);

    // dp3 o1.xyzw, o0.xyzw, o1.xyzw
    let mut dp3_o1 = vec![opcode_token(OPCODE_DP3, 0)];
    dp3_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    dp3_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    dp3_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    dp3_o1[0] = opcode_token(OPCODE_DP3, dp3_o1.len() as u32);
    tokens.extend_from_slice(&dp3_o1);

    // dp4 o0.xyzw, o0.xyzw, o1.xyzw
    let mut dp4_o0 = vec![opcode_token(OPCODE_DP4, 0)];
    dp4_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    dp4_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    dp4_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    dp4_o0[0] = opcode_token(OPCODE_DP4, dp4_o0.len() as u32);
    tokens.extend_from_slice(&dp4_o0);

    // min o0.xyzw, o0.xyzw, l(0, 0, 0, 0)
    let mut min_o0 = vec![opcode_token(OPCODE_MIN, 0)];
    min_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    min_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    min_o0.extend_from_slice(&imm32_vec4([0; 4]));
    min_o0[0] = opcode_token(OPCODE_MIN, min_o0.len() as u32);
    tokens.extend_from_slice(&min_o0);

    // max o1.xyzw, o1.xyzw, l(0, 0, 0, 0)
    let mut max_o1 = vec![opcode_token(OPCODE_MAX, 0)];
    max_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    max_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    max_o1.extend_from_slice(&imm32_vec4([0; 4]));
    max_o1[0] = opcode_token(OPCODE_MAX, max_o1.len() as u32);
    tokens.extend_from_slice(&max_o1);

    // emit; ret
    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains(") * ("),
        "expected mul/mad to translate to a parenthesized multiply expression:\n{wgsl}"
    );
    assert!(
        wgsl.contains("dot(("),
        "expected dp3/dp4 to translate via WGSL dot() intrinsic:\n{wgsl}"
    );
    assert!(
        wgsl.contains("min(("),
        "expected min to translate via WGSL min() intrinsic:\n{wgsl}"
    );
    assert!(
        wgsl.contains("max(("),
        "expected max to translate via WGSL max() intrinsic:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_pointlist_output_topology_translates_to_wgsl_compute_prepass() {
    // Minimal gs_4_0 token stream with pointlist output:
    // - dcl_inputprimitive point
    // - dcl_outputtopology pointlist
    // - dcl_maxvertexcount 1
    // - mov o0, v0[0]; emit; ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(1); // point
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(1); // pointlist
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_INPUT, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // ret
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("// Point list index emission."),
        "expected point list index emission path in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_indices.data[base] = vtx_idx;"),
        "expected point list to emit one index per vertex:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_linestrip_output_topology_translates_to_wgsl_compute_prepass() {
    // Minimal gs_4_0 token stream with linestrip output (tokenized-format encoding):
    // - dcl_inputprimitive line
    // - dcl_outputtopology linestrip
    // - dcl_maxvertexcount 4
    // - emit two vertices, cut, emit two vertices, ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(2); // line
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(2); // linestrip (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(4);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_INPUT, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // cut
    tokens.push(opcode_token(OPCODE_CUT, 1));

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // ret
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("// Line strip -> line list index emission."),
        "expected line strip index emission path in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_indices.data[base] = *strip_prev0;"),
        "expected line strip to emit line-list indices:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_indices.data[base + 1u] = vtx_idx;"),
        "expected line strip to emit pairs of indices:\n{wgsl}"
    );
    assert!(
        wgsl.contains("gs_cut(&strip_len)"),
        "expected cut lowering to reset strip_len:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_linestrip_output_topology_d3d_encoding_translates() {
    // Some toolchains encode `dcl_outputtopology` using D3D primitive topology constants.
    // For linestrip that means `3` (D3D10_PRIMITIVE_TOPOLOGY_LINESTRIP).
    //
    // Use a triangle input encoded as `4` (D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST) so the translator
    // can infer the encoding style and disambiguate output_topology=3 (line strip vs triangle strip).
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(4); // triangle (D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // linestrip (D3D10_PRIMITIVE_TOPOLOGY_LINESTRIP)
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(2);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_INPUT, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // ret
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("// Line strip -> line list index emission."),
        "expected d3d-encoded line strip output topology to translate:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_emit_cut_fixture_translates() {
    // The checked-in fixture uses D3D10 tokenized-program-format encodings for GS declarations
    // (e.g. output topology triangle_strip=5), which differ from the small synthetic token streams
    // in this test (which use "tokenized shader format" enums like triangle_strip=3).
    //
    // The GS prepass translator should accept these encodings so it can run real DXBC blobs
    // produced by various toolchains.
    const DXBC: &[u8] = include_bytes!("fixtures/gs_cut.dxbc");

    let program = Sm4Program::parse_from_dxbc_bytes(DXBC).expect("SM4 parse");
    assert_eq!(program.stage, ShaderStage::Geometry);
    assert_eq!(program.model, ShaderModel { major: 4, minor: 0 });

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("GS_MAX_VERTEX_COUNT"),
        "expected generated WGSL to include max vertex count constant"
    );
    assert!(
        wgsl.contains("arrayLength(&out_vertices.data)"),
        "expected generated WGSL to bounds-check out_vertices"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_parallelizes_cs_main_and_uses_atomics() {
    const DXBC: &[u8] = include_bytes!("fixtures/gs_emit_cut.dxbc");

    let program = Sm4Program::parse_from_dxbc_bytes(DXBC).expect("SM4 parse");
    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains(
            "fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n  let prim_id: u32 = id.x;"
        ),
        "expected cs_main to treat global_invocation_id.x as prim_id (no single-thread guard):\n{wgsl}"
    );
    assert!(
        !wgsl.contains("for (var prim_id: u32 = 0u; prim_id < params.primitive_count"),
        "expected cs_main to process exactly one primitive per invocation (no prim_id loop):\n{wgsl}"
    );
    assert!(
        wgsl.contains("atomicAdd"),
        "expected translated WGSL to use atomic counters for append allocation:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm5_gs_emit_stream_cut_stream_fixture_rejects_nonzero_stream() {
    const DXBC: &[u8] = include_bytes!("fixtures/gs_emit_stream_cut_stream.dxbc");

    let program = Sm4Program::parse_from_dxbc_bytes(DXBC).expect("SM4 parse");
    assert_eq!(program.stage, ShaderStage::Geometry);
    assert_eq!(program.model, ShaderModel { major: 5, minor: 0 });

    let module = decode_program(&program).expect("decode");
    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject non-zero stream indices");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedStream {
            inst_index: 0,
            opcode: "emit_stream",
            stream: 2
        }
    );
}

#[test]
fn sm4_gs_emitthen_cut_translates_to_wgsl_compute_prepass() {
    // Minimal gs_4_0 token stream with `emitthen_cut` on stream 0.
    //
    // - dcl_inputprimitive triangle
    // - dcl_outputtopology triangle_strip
    // - dcl_maxvertexcount 1
    // - mov o0, v0[0]
    // - emitthen_cut
    // - ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // D3D10_SB_PRIMITIVE_TRIANGLE
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(5); // D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_INPUT, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(OPCODE_DCL_OUTPUT, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emitthen_cut (stream 0)
    tokens.push(opcode_token(OPCODE_EMITTHENCUT, 1));

    // ret
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("fn gs_emit"),
        "expected generated WGSL to contain gs_emit helper function"
    );
    assert!(
        wgsl.contains("fn gs_cut"),
        "expected generated WGSL to contain gs_cut helper function"
    );
    assert!(
        wgsl.contains("gs_emit(o0,"),
        "expected generated WGSL to call gs_emit:\n{wgsl}"
    );
    assert!(
        wgsl.contains("gs_cut(&strip_len)"),
        "expected generated WGSL to call gs_cut"
    );
    assert!(
        wgsl.contains("// emitthen_cut"),
        "expected generated WGSL to tag emitthen_cut lowering"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm5_gs_instance_id_translates_to_wgsl_compute_prepass() {
    // D3D name token for `SV_GSInstanceID`.
    const D3D_NAME_GS_INSTANCE_ID: u32 = 11;
    const DCL_DUMMY: u32 = 0x100;

    let version_token = 0x0003_0050u32; // nominal gs_5_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // triangle (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    // Use the D3D primitive-topology constant here to ensure the translator tolerates both
    // tokenized-format and topology-style encodings.
    tokens.push(5); // triangle_strip
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);
    tokens.push(opcode_token(OPCODE_DCL_GS_INSTANCE_COUNT, 2));
    tokens.push(2);

    // dcl_input_siv v0.x, SV_GSInstanceID
    tokens.push(opcode_token(DCL_DUMMY, 4));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::X));
    tokens.push(D3D_NAME_GS_INSTANCE_ID);

    // dcl_output o0.xyzw
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    // dcl_output o1.xyzw
    tokens.push(opcode_token(DCL_DUMMY + 2, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));

    // mov o1.xyzw, v0.x
    tokens.push(opcode_token(OPCODE_MOV, 5));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, 0));

    // emit; ret
    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 5, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("const GS_INSTANCE_COUNT: u32 = 2u;"),
        "expected GS instance count to be reflected in WGSL constants"
    );
    assert!(
        wgsl.contains("gs_instance_id"),
        "expected generated WGSL to reference gs_instance_id system value"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_mul_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // mul o0.xyzw, l(1,2,3,4), l(5,6,7,8)
    tokens.push(opcode_token(OPCODE_MUL, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0x40a00000); // 5.0
    tokens.push(0x40c00000); // 6.0
    tokens.push(0x40e00000); // 7.0
    tokens.push(0x41000000); // 8.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(") * ("),
        "expected generated WGSL to contain a mul expression:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_mul_respects_swizzle_modifier_and_saturate() {
    let mut tokens = base_gs_tokens();

    // mov r0.xyzw, l(1, 2, 3, 4)
    let mut mov_r0 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_r0.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov_r0.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
    ]));
    mov_r0[0] = opcode_token(OPCODE_MOV, mov_r0.len() as u32);
    tokens.extend_from_slice(&mov_r0);

    // mov r1.xyzw, l(5, 6, 7, 8)
    let mut mov_r1 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_r1.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    mov_r1.extend_from_slice(&imm32_vec4([
        5.0f32.to_bits(),
        6.0f32.to_bits(),
        7.0f32.to_bits(),
        8.0f32.to_bits(),
    ]));
    mov_r1[0] = opcode_token(OPCODE_MOV, mov_r1.len() as u32);
    tokens.extend_from_slice(&mov_r1);

    // mul_sat o0.xyzw, -r0.wzyx, abs(r1.zyxw)
    let mut mul_o0 = vec![opcode_token(OPCODE_MUL, 0) | OPCODE_EXTENDED_BIT];
    // Extended opcode token (type=0) with saturate bit set (bit 13).
    mul_o0.push(1u32 << 13);
    mul_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    mul_o0.extend_from_slice(&reg_src_swizzle_modifier(
        OPERAND_TYPE_TEMP,
        0,
        [3, 2, 1, 0],
        1,
    ));
    mul_o0.extend_from_slice(&reg_src_swizzle_modifier(
        OPERAND_TYPE_TEMP,
        1,
        [2, 1, 0, 3],
        2,
    ));
    mul_o0[0] = opcode_token(OPCODE_MUL, mul_o0.len() as u32) | OPCODE_EXTENDED_BIT;
    tokens.extend_from_slice(&mul_o0);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(".wzyx"),
        "expected src swizzle to be preserved:\n{wgsl}"
    );
    assert!(
        wgsl.contains("abs("),
        "expected abs modifier to be preserved:\n{wgsl}"
    );
    assert!(
        wgsl.contains("clamp(("),
        "expected saturate flag to lower to clamp():\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_mad_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // mad o0.xyzw, l(1,1,1,1), l(2,2,2,2), l(3,3,3,3)
    tokens.push(opcode_token(OPCODE_MAD, 18));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.extend([0x3f800000; 4]); // 1.0
    tokens.push(0x42); // imm
    tokens.extend([0x40000000; 4]); // 2.0
    tokens.push(0x42); // imm
    tokens.extend([0x40400000; 4]); // 3.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(") * (") && wgsl.contains(") + ("),
        "expected generated WGSL to contain a mad expression:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_min_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // min o0.xyzw, l(1,2,3,4), l(4,3,2,1)
    tokens.push(opcode_token(OPCODE_MIN, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40800000); // 4.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x3f800000); // 1.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("min(("),
        "expected generated WGSL to contain a min() call:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_max_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // max o0.xyzw, l(1,2,3,4), l(4,3,2,1)
    tokens.push(opcode_token(OPCODE_MAX, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40800000); // 4.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x3f800000); // 1.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("max(("),
        "expected generated WGSL to contain a max() call:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_dp3_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // dp3 o0.x, l(1,2,3,4), l(5,6,7,8)
    tokens.push(opcode_token(OPCODE_DP3, 13));
    tokens.push(0x101022); // o0.x (mask mode, component_sel=1)
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40a00000); // 5.0
    tokens.push(0x40c00000); // 6.0
    tokens.push(0x40e00000); // 7.0
    tokens.push(0x41000000); // 8.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("dot((") && wgsl.contains(".xyz"),
        "expected generated WGSL to contain a dp3 dot() call:\n{wgsl}"
    );
    assert!(
        wgsl.contains("o0.x ="),
        "expected write-mask to lower to component assignment:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_dp4_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // dp4 o0.xyzw, l(1,2,3,4), l(5,6,7,8)
    tokens.push(opcode_token(OPCODE_DP4, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40a00000); // 5.0
    tokens.push(0x40c00000); // 6.0
    tokens.push(0x40e00000); // 7.0
    tokens.push(0x41000000); // 8.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("vec4<f32>(dot(("),
        "expected generated WGSL to contain a dp4 dot() call:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_movc_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // movc o0.xyzw, l(1,0,0,0), l(2,2,2,2), l(3,3,3,3)
    tokens.push(opcode_token(OPCODE_MOVC, 18));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // cond imm
    tokens.push(0x3f800000); // 1.0 (non-zero => true)
    tokens.push(0);
    tokens.push(0);
    tokens.push(0);
    tokens.push(0x42); // a imm
    tokens.extend([0x40000000; 4]); // 2.0
    tokens.push(0x42); // b imm
    tokens.extend([0x40400000; 4]); // 3.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("select(("),
        "expected generated WGSL to contain a select() call for movc:\n{wgsl}"
    );
    assert!(
        wgsl.contains("!= vec4<u32>(0u)"),
        "expected movc condition to be implemented via bitcast/!=0:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_movc_respects_saturate() {
    let mut tokens = base_gs_tokens();

    // movc_sat o0.xyzw, l(0,1,0,1), l(2,2,2,2), l(-1,-1,-1,-1)
    //
    // This test is intentionally string-based: it ensures the GS prepass translator:
    // - lowers movc via WGSL `select` with a vector boolean condition
    // - applies the saturate flag via `clamp` *around* the select expression
    let mut inst = vec![opcode_token(OPCODE_MOVC, 0) | OPCODE_EXTENDED_BIT];
    // Extended opcode token (type=0) with saturate bit set (bit 13).
    inst.push(1u32 << 13);
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    inst.extend_from_slice(&imm32_vec4([2.0f32.to_bits(); 4]));
    inst.extend_from_slice(&imm32_vec4([(-1.0f32).to_bits(); 4]));
    inst[0] = opcode_token(OPCODE_MOVC, inst.len() as u32) | OPCODE_EXTENDED_BIT;
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("select(("),
        "expected generated WGSL to contain a select() call for movc:\n{wgsl}"
    );
    assert!(
        wgsl.contains("!= vec4<u32>(0u)"),
        "expected movc condition to be implemented via bitcast/!=0:\n{wgsl}"
    );
    assert!(
        wgsl.contains("clamp((select(("),
        "expected saturate flag to wrap the movc select() via clamp():\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_itof_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // itof o0.xyzw, l(1, -2, 3, 0)
    let mut inst = vec![opcode_token(OPCODE_ITOF, 0)];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([1u32, (-2i32) as u32, 3u32, 0u32]));
    inst[0] = opcode_token(OPCODE_ITOF, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("vec4<f32>(vec4<i32>("),
        "expected itof to lower via WGSL i32->f32 conversion:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_utof_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // utof o0.xyzw, l(0, 1, 2, 0xffffffff)
    let mut inst = vec![opcode_token(OPCODE_UTOF, 0)];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([0u32, 1u32, 2u32, 0xffff_ffff]));
    inst[0] = opcode_token(OPCODE_UTOF, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("vec4<f32>(vec4<u32>("),
        "expected utof to lower via WGSL u32->f32 conversion:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_ftoi_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // ftoi o0.xyzw, l(1.0, -2.0, 3.5, 0.0)
    let mut inst = vec![opcode_token(OPCODE_FTOI, 0)];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        (-2.0f32).to_bits(),
        3.5f32.to_bits(),
        0.0f32.to_bits(),
    ]));
    inst[0] = opcode_token(OPCODE_FTOI, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("bitcast<vec4<f32>>(vec4<i32>("),
        "expected ftoi to lower via WGSL vec4<i32>(...) + bitcast back to vec4<f32>:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_ftou_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // ftou o0.xyzw, l(1.0, 2.0, 3.5, 0.0)
    let mut inst = vec![opcode_token(OPCODE_FTOU, 0)];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.5f32.to_bits(),
        0.0f32.to_bits(),
    ]));
    inst[0] = opcode_token(OPCODE_FTOU, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("bitcast<vec4<f32>>(vec4<u32>("),
        "expected ftou to lower via WGSL vec4<u32>(...) + bitcast back to vec4<f32>:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_rcp_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // rcp o0.xyzw, l(2, 4, 8, 16)
    let mut inst = vec![opcode_token(OPCODE_RCP, 0)];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([
        2.0f32.to_bits(),
        4.0f32.to_bits(),
        8.0f32.to_bits(),
        16.0f32.to_bits(),
    ]));
    inst[0] = opcode_token(OPCODE_RCP, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("1.0 / ("),
        "expected rcp to lower to reciprocal divide:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_rsq_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // rsq o0.xyzw, l(4, 9, 16, 25)
    let mut inst = vec![opcode_token(OPCODE_RSQ, 0)];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([
        4.0f32.to_bits(),
        9.0f32.to_bits(),
        16.0f32.to_bits(),
        25.0f32.to_bits(),
    ]));
    inst[0] = opcode_token(OPCODE_RSQ, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("inverseSqrt("),
        "expected rsq to lower to WGSL inverseSqrt():\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_and_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // and o0.xyzw, l(0xffffffff, 0, 0xffffffff, 0), l(0, 0xffffffff, 0, 0xffffffff)
    let mut inst = vec![opcode_token(OPCODE_AND, 0)];
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([0xffff_ffff, 0, 0xffff_ffff, 0]));
    inst.extend_from_slice(&imm32_vec4([0, 0xffff_ffff, 0, 0xffff_ffff]));
    inst[0] = opcode_token(OPCODE_AND, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(") & ("),
        "expected and to lower to WGSL bitwise &:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_rejects_regfile_output_depth_source() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 3 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::OutputDepth,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject RegFile::OutputDepth sources");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedOperand {
            inst_index: 0,
            opcode: "mov",
            msg: "RegFile::OutputDepth is not supported in GS prepass".to_owned()
        }
    );
}

#[test]
fn gs_translate_rejects_regfile_input_without_siv_decl() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 3 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Input,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject RegFile::Input without dcl_input_siv");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedOperand {
            inst_index: 0,
            opcode: "mov",
            msg: "unsupported input register v0 (expected v#[]/SrcKind::GsInput or a supported system value via dcl_input_siv)".to_owned()
        }
    );
}

#[test]
fn gs_translate_rejects_regfile_output_depth_destination() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 3 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::OutputDepth,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject RegFile::OutputDepth destinations");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedOperand {
            inst_index: 0,
            opcode: "mov",
            msg: "unsupported destination register file RegFile::OutputDepth".to_owned()
        }
    );
}

#[test]
fn gs_translate_supports_structured_control_flow_if_else_loop_break_continue_breakc_continuec() {
    const D3D_NAME_PRIMITIVE_ID: u32 = 7;

    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::X,
                sys_value: D3D_NAME_PRIMITIVE_ID,
            },
        ],
        instructions: vec![
            // Set up outputs (required by emit).
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0, 0, 0, 0x3f800000]), // (0,0,0,1)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 1,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0x3f800000, 0, 0, 0x3f800000]), // (1,0,0,1)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            // if (primitive_id != 0) { emit } else { cut }
            Sm4Inst::If {
                cond: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Input,
                        index: 0,
                    }),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Else,
            Sm4Inst::Cut { stream: 0 },
            Sm4Inst::EndIf,
            // loop { continuec; breakc; }
            Sm4Inst::Loop,
            Sm4Inst::ContinueC {
                op: Sm4CmpOp::Eq,
                a: SrcOperand {
                    kind: SrcKind::ImmediateF32([0x3f800000; 4]), // 1.0
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                b: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]), // 0.0
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::BreakC {
                op: Sm4CmpOp::Eq,
                a: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]), // 0.0
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                b: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]), // 0.0
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::EndLoop,
            // loop { ifc { continue; } break; }
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::X,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]), // 0.0
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Loop,
            Sm4Inst::Add {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::X,
                    saturate: false,
                },
                a: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                b: SrcOperand {
                    kind: SrcKind::ImmediateF32([0x3f800000; 4]), // 1.0
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::IfC {
                op: Sm4CmpOp::Eq,
                a: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                b: SrcOperand {
                    kind: SrcKind::ImmediateF32([0x3f800000; 4]), // 1.0
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Continue,
            Sm4Inst::EndIf,
            Sm4Inst::Break,
            Sm4Inst::EndLoop,
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("if ("),
        "expected translated WGSL to contain an if statement:\n{wgsl}"
    );
    assert!(
        wgsl.contains("} else {"),
        "expected translated WGSL to contain an else clause:\n{wgsl}"
    );
    assert!(
        wgsl.contains("loop {"),
        "expected translated WGSL to contain a loop statement:\n{wgsl}"
    );
    assert!(
        wgsl.contains("break;"),
        "expected translated WGSL to contain a break statement:\n{wgsl}"
    );
    assert!(
        wgsl.contains("continue;"),
        "expected translated WGSL to contain a continue statement:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_supports_switch_case_default() {
    const D3D_NAME_PRIMITIVE_ID: u32 = 7;

    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::X,
                sys_value: D3D_NAME_PRIMITIVE_ID,
            },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0, 0, 0, 0x3f800000]), // (0,0,0,1)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 1,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0x3f800000, 0, 0, 0x3f800000]), // (1,0,0,1)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Switch {
                selector: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Input,
                        index: 0,
                    }),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Case { value: 0 },
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Break,
            Sm4Inst::Default,
            Sm4Inst::Cut { stream: 0 },
            Sm4Inst::EndSwitch,
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("switch("),
        "expected translated WGSL to contain a switch statement:\n{wgsl}"
    );
    assert!(
        wgsl.contains("case 0i:"),
        "expected translated WGSL to contain a case label:\n{wgsl}"
    );
    assert!(
        wgsl.contains("default:"),
        "expected translated WGSL to contain a default label:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_supports_breakc_inside_switch_case() {
    const D3D_NAME_PRIMITIVE_ID: u32 = 7;

    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::X,
                sys_value: D3D_NAME_PRIMITIVE_ID,
            },
        ],
        instructions: vec![
            Sm4Inst::Switch {
                selector: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Input,
                        index: 0,
                    }),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Case { value: 0 },
            Sm4Inst::BreakC {
                op: Sm4CmpOp::Eq,
                a: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                b: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::EndSwitch,
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");
    assert!(
        wgsl.contains("switch("),
        "expected switch statement:\n{wgsl}"
    );
    assert!(wgsl.contains("case 0i:"), "expected case label:\n{wgsl}");
    assert!(
        wgsl.contains("if (") && wgsl.contains("break;"),
        "expected conditional break in WGSL:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_supports_constant_buffer_operands() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Point(1),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(5),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::ConstantBuffer {
                slot: 0,
                reg_count: 1,
            },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 1,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ConstantBuffer { slot: 0, reg: 0 },
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Emit { stream: 0 },
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");
    assert!(
        wgsl.contains("@group(3) @binding(0)"),
        "expected cbuffer binding to be emitted in group(3):\n{wgsl}"
    );
    assert!(
        wgsl.contains("struct Cb0"),
        "expected cbuffer struct declaration to be emitted:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_half_float_conversions_translate_via_pack_unpack() {
    // Ensure the GS compute-prepass translator supports `f32tof16`/`f16tof32` without requiring
    // WGSL `f16` types (use pack/unpack builtins).
    let mut tokens = base_gs_tokens();

    // mov r0.xyzw, l(1,2,3,4)
    tokens.push(opcode_token(OPCODE_MOV, 8));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    tokens.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
    ]));

    // f32tof16 r1.xyzw, r0.xyzw
    tokens.push(opcode_token(OPCODE_F32TOF16, 5));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));

    // f16tof32 r2.xyzw, r1.xyzw
    tokens.push(opcode_token(OPCODE_F16TOF32, 5));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert_wgsl_validates(&wgsl);
    assert!(
        wgsl.contains("pack2x16float"),
        "expected f32tof16 lowering to use pack2x16float:\n{wgsl}"
    );
    assert!(
        wgsl.contains("unpack2x16float"),
        "expected f16tof32 lowering to use unpack2x16float:\n{wgsl}"
    );
    assert!(
        wgsl.contains("& 0xffffu"),
        "expected half-float conversions to mask low 16 bits:\n{wgsl}"
    );
}

#[test]
fn sm4_gs_f32tof16_sat_clamps_input_before_packing() {
    // `f32tof16_sat` should clamp float values to 0..1 *before* converting to half-float bits.
    let mut tokens = base_gs_tokens();

    // mov r0.xyzw, l(2.0, -1.0, 0.5, 42.0)
    tokens.push(opcode_token(OPCODE_MOV, 8));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    tokens.extend_from_slice(&imm32_vec4([
        2.0f32.to_bits(),
        (-1.0f32).to_bits(),
        0.5f32.to_bits(),
        42.0f32.to_bits(),
    ]));

    // f32tof16_sat r1.xyzw, r0.xyzw
    //
    // Extended opcode token (type 0) with saturate bit set at bit 13.
    tokens.push(opcode_token(OPCODE_F32TOF16, 6) | OPCODE_EXTENDED_BIT);
    tokens.push(1u32 << 13);
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));

    // f16tof32 r2.xyzw, r1.xyzw
    tokens.push(opcode_token(OPCODE_F16TOF32, 5));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert_wgsl_validates(&wgsl);
    assert!(
        wgsl.contains("clamp(("),
        "expected f32tof16_sat lowering to clamp float values:\n{wgsl}"
    );
    assert!(
        wgsl.contains("pack2x16float"),
        "expected f32tof16 lowering to use pack2x16float:\n{wgsl}"
    );
    assert!(
        wgsl.contains("& 0xffffu"),
        "expected f32tof16 lowering to mask low 16 bits:\n{wgsl}"
    );
    assert!(
        wgsl.contains("unpack2x16float"),
        "expected f16tof32 lowering to use unpack2x16float:\n{wgsl}"
    );
}

#[test]
fn sm4_gs_f16tof32_ignores_operand_modifier_to_preserve_half_bits() {
    // `f16tof32` consumes raw binary16 payloads stored in the low 16 bits of untyped DXBC register
    // lanes. Operand modifiers (e.g. -abs) would reinterpret the lane numerically and corrupt the
    // packed half bits, so the GS compute-prepass translator must ignore them.
    let mut tokens = base_gs_tokens();

    // mov r0.xyzw, l(1,0,0,0)
    let mut mov_r0 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_r0.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov_r0.extend_from_slice(&imm32_vec4([1.0f32.to_bits(), 0, 0, 0]));
    mov_r0[0] = opcode_token(OPCODE_MOV, mov_r0.len() as u32);
    tokens.extend_from_slice(&mov_r0);

    // f32tof16 r1.xyzw, r0.xyzw
    tokens.push(opcode_token(OPCODE_F32TOF16, 5));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));

    // f16tof32 r2.xyzw, -r1.xyzw
    //
    // Operand modifier encoding:
    // - 1 = neg
    tokens.push(opcode_token(OPCODE_F16TOF32, 6));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src_swizzle_modifier(
        OPERAND_TYPE_TEMP,
        1,
        Swizzle::XYZW.0,
        1,
    ));

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert_wgsl_validates(&wgsl);
    assert!(
        wgsl.contains("unpack2x16float"),
        "expected f16tof32 lowering to use unpack2x16float:\n{wgsl}"
    );
    assert!(
        !wgsl.contains("vec4<u32>(0u) -"),
        "expected f16tof32 to ignore source operand modifiers (preserve half bits):\n{wgsl}"
    );
}

#[test]
fn gs_translate_supports_setp_and_predicated_emit_cut() {
    const D3D_NAME_PRIMITIVE_ID: u32 = 7;

    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 1 },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::X,
                sys_value: D3D_NAME_PRIMITIVE_ID,
            },
            // Keep the module realistic: `decode_program` normally emits these from `dcl_output`
            // tokens, and the GS translator uses them to determine which varyings to export.
            Sm4Decl::Output {
                reg: 0,
                mask: WriteMask::XYZW,
            },
            Sm4Decl::Output {
                reg: 1,
                mask: WriteMask::XYZW,
            },
        ],
        instructions: vec![
            // Set up outputs (required by emit).
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0, 0, 0, 0x3f800000]), // (0,0,0,1)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 1,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0x3f800000, 0, 0, 0x3f800000]), // (1,0,0,1)
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            // setp p0.x, primitive_id, 0 (`*_U` is an unordered float compare in SM4)
            Sm4Inst::Setp {
                dst: PredicateDstOperand {
                    reg: PredicateRef { index: 0 },
                    mask: WriteMask::X,
                },
                op: Sm4CmpOp::LtU,
                a: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Input,
                        index: 0,
                    }),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                b: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
            },
            // (+p0.x) emit; (-p0.x) cut
            Sm4Inst::Predicated {
                pred: PredicateOperand {
                    reg: PredicateRef { index: 0 },
                    component: 0,
                    invert: false,
                },
                inner: Box::new(Sm4Inst::Emit { stream: 0 }),
            },
            Sm4Inst::Predicated {
                pred: PredicateOperand {
                    reg: PredicateRef { index: 0 },
                    component: 0,
                    invert: true,
                },
                inner: Box::new(Sm4Inst::Cut { stream: 0 }),
            },
            Sm4Inst::Ret,
        ],
    };

    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("var p0: vec4<bool>"),
        "expected translated WGSL to declare predicate register p0:\n{wgsl}"
    );
    assert!(
        wgsl.contains("if (p0.x) {"),
        "expected translated WGSL to wrap predicated emit in an if:\n{wgsl}"
    );
    assert!(
        wgsl.contains("if (!(p0.x)) {"),
        "expected translated WGSL to wrap inverted predicated cut in an if:\n{wgsl}"
    );
    assert!(
        wgsl.contains("gs_emit(o0, o1"),
        "expected translated WGSL to still call gs_emit:\n{wgsl}"
    );
    assert!(
        wgsl.contains("gs_cut(&strip_len)"),
        "expected translated WGSL to still call gs_cut:\n{wgsl}"
    );
    assert!(
        wgsl.contains("!= ((setp_a_") || wgsl.contains("!= ((setp_b_"),
        "expected unordered setp (`*_U`) to include NaN handling (`x != x`):\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_integer_bitwise_ops_translate_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // iadd r0.xyzw, r1.xyzw, r2.xyzw
    let mut iadd = vec![opcode_token(OPCODE_IADD, 0)];
    iadd.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    iadd.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    iadd.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    iadd[0] = opcode_token(OPCODE_IADD, iadd.len() as u32);
    tokens.extend_from_slice(&iadd);

    // isub r3.xyzw, r0.xyzw, r2.xyzw
    let mut isub = vec![opcode_token(OPCODE_ISUB, 0)];
    isub.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    isub.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    isub.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    isub[0] = opcode_token(OPCODE_ISUB, isub.len() as u32);
    tokens.extend_from_slice(&isub);

    // or r4.xyzw, r0.xyzw, r2.xyzw
    let mut or_inst = vec![opcode_token(OPCODE_OR, 0)];
    or_inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    or_inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    or_inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    or_inst[0] = opcode_token(OPCODE_OR, or_inst.len() as u32);
    tokens.extend_from_slice(&or_inst);

    // xor r5.xyzw, r4.xyzw, r2.xyzw
    let mut xor_inst = vec![opcode_token(OPCODE_XOR, 0)];
    xor_inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    xor_inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 4));
    xor_inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    xor_inst[0] = opcode_token(OPCODE_XOR, xor_inst.len() as u32);
    tokens.extend_from_slice(&xor_inst);

    // not r6.xyzw, r5.xyzw
    let mut not_inst = vec![opcode_token(OPCODE_NOT, 0)];
    not_inst.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    not_inst.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 5));
    not_inst[0] = opcode_token(OPCODE_NOT, not_inst.len() as u32);
    tokens.extend_from_slice(&not_inst);

    // emit; ret
    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("bitcast<vec4<i32>>"),
        "expected integer ops to bitcast sources through vec4<i32>:\n{wgsl}"
    );
    assert!(
        wgsl.contains(" | "),
        "expected OR lowering to use the bitwise | operator:\n{wgsl}"
    );
    assert!(
        wgsl.contains(" ^ "),
        "expected XOR lowering to use the bitwise ^ operator:\n{wgsl}"
    );
    assert!(
        wgsl.contains("~("),
        "expected NOT lowering to use the bitwise ~ operator:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_shift_ops_translate_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // ishl r0.xyzw, r1.xyzw, r2.xyzw
    let mut ishl = vec![opcode_token(OPCODE_ISHL, 0)];
    ishl.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    ishl.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    ishl.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    ishl[0] = opcode_token(OPCODE_ISHL, ishl.len() as u32);
    tokens.extend_from_slice(&ishl);

    // ishr r3.xyzw, r0.xyzw, r2.xyzw
    let mut ishr = vec![opcode_token(OPCODE_ISHR, 0)];
    ishr.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    ishr.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    ishr.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    ishr[0] = opcode_token(OPCODE_ISHR, ishr.len() as u32);
    tokens.extend_from_slice(&ishr);

    // ushr r4.xyzw, r0.xyzw, r2.xyzw
    let mut ushr = vec![opcode_token(OPCODE_USHR, 0)];
    ushr.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    ushr.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    ushr.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    ushr[0] = opcode_token(OPCODE_USHR, ushr.len() as u32);
    tokens.extend_from_slice(&ushr);

    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(" << ("),
        "expected shift-left lowering to use the << operator:\n{wgsl}"
    );
    assert!(
        wgsl.contains(" >> ("),
        "expected shift-right lowering to use the >> operator:\n{wgsl}"
    );
    assert!(
        wgsl.contains("vec4<u32>(31u)"),
        "expected DXBC shift mask (31u) to be applied:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_integer_min_max_abs_neg_translate_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // imin r0.xyzw, r1.xyzw, r2.xyzw
    let mut imin = vec![opcode_token(OPCODE_IMIN, 0)];
    imin.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    imin.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    imin.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    imin[0] = opcode_token(OPCODE_IMIN, imin.len() as u32);
    tokens.extend_from_slice(&imin);

    // imax r1.xyzw, r0.xyzw, r2.xyzw
    let mut imax = vec![opcode_token(OPCODE_IMAX, 0)];
    imax.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    imax.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    imax.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    imax[0] = opcode_token(OPCODE_IMAX, imax.len() as u32);
    tokens.extend_from_slice(&imax);

    // umin r2.xyzw, r0.xyzw, r1.xyzw
    let mut umin = vec![opcode_token(OPCODE_UMIN, 0)];
    umin.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    umin.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    umin.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    umin[0] = opcode_token(OPCODE_UMIN, umin.len() as u32);
    tokens.extend_from_slice(&umin);

    // umax r3.xyzw, r0.xyzw, r1.xyzw
    let mut umax = vec![opcode_token(OPCODE_UMAX, 0)];
    umax.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 3, WriteMask::XYZW));
    umax.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    umax.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    umax[0] = opcode_token(OPCODE_UMAX, umax.len() as u32);
    tokens.extend_from_slice(&umax);

    // iabs r4.xyzw, r0.xyzw
    let mut iabs = vec![opcode_token(OPCODE_IABS, 0)];
    iabs.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    iabs.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    iabs[0] = opcode_token(OPCODE_IABS, iabs.len() as u32);
    tokens.extend_from_slice(&iabs);

    // ineg r5.xyzw, r0.xyzw
    let mut ineg = vec![opcode_token(OPCODE_INEG, 0)];
    ineg.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    ineg.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 0));
    ineg[0] = opcode_token(OPCODE_INEG, ineg.len() as u32);
    tokens.extend_from_slice(&ineg);

    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("vec4<u32>(min(("),
        "expected imin lowering to use vec4<u32>(min(...)):\n{wgsl}"
    );
    assert!(
        wgsl.contains("vec4<u32>(max(("),
        "expected imax lowering to use vec4<u32>(max(...)):\n{wgsl}"
    );
    assert!(
        wgsl.contains("vec4<u32>(abs("),
        "expected iabs lowering to use vec4<u32>(abs(...)):\n{wgsl}"
    );
    assert!(
        wgsl.contains("vec4<u32>(-("),
        "expected ineg lowering to use vec4<u32>(-(...)):\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_cmp_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // lt r0.xyzw, r1.xyzw, r2.xyzw  (float compare -> predicate mask bits)
    let mut lt = vec![opcode_token(OPCODE_LT, 0)];
    lt.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    lt.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    lt.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    lt[0] = opcode_token(OPCODE_LT, lt.len() as u32);
    tokens.extend_from_slice(&lt);

    // ilt r1.xyzw, r1.xyzw, r2.xyzw (signed int compare)
    let mut ilt = vec![opcode_token(OPCODE_ILT, 0)];
    ilt.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    ilt.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    ilt.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    ilt[0] = opcode_token(OPCODE_ILT, ilt.len() as u32);
    tokens.extend_from_slice(&ilt);

    // ult r2.xyzw, r1.xyzw, r2.xyzw (unsigned int compare)
    let mut ult = vec![opcode_token(OPCODE_ULT, 0)];
    ult.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::XYZW));
    ult.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    ult.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    ult[0] = opcode_token(OPCODE_ULT, ult.len() as u32);
    tokens.extend_from_slice(&ult);

    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("0xffffffffu"),
        "expected cmp lowering to use 0xffffffffu predicate mask values:\n{wgsl}"
    );
    assert!(
        wgsl.contains("select(vec4<u32>(0u), vec4<u32>(0xffffffffu)"),
        "expected cmp lowering to use select() to build predicate masks:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_udiv_idiv_translate_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // udiv r0.xyzw, r1.xyzw, r2.xyzw, r3.xyzw
    let mut udiv = vec![opcode_token(OPCODE_UDIV, 0)];
    udiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    udiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    udiv.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    udiv.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    udiv[0] = opcode_token(OPCODE_UDIV, udiv.len() as u32);
    tokens.extend_from_slice(&udiv);

    // idiv r4.xyzw, r5.xyzw, r2.xyzw, r3.xyzw
    let mut idiv = vec![opcode_token(OPCODE_IDIV, 0)];
    idiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    idiv.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    idiv.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    idiv.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    idiv[0] = opcode_token(OPCODE_IDIV, idiv.len() as u32);
    tokens.extend_from_slice(&idiv);

    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("let udiv_q"),
        "expected udiv lowering to introduce quotient temporaries:\n{wgsl}"
    );
    assert!(
        wgsl.contains(" % "),
        "expected div lowering to compute a remainder with %:\n{wgsl}"
    );
    assert!(
        wgsl.contains("let idiv_q"),
        "expected idiv lowering to introduce quotient temporaries:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_bitfield_ops_translate_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // bfi r0.xyzw, r1.xyzw, r2.xyzw, r3.xyzw, r4.xyzw
    let mut bfi = vec![opcode_token(OPCODE_BFI, 0)];
    bfi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    bfi.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    bfi.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    bfi.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    bfi.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 4));
    bfi[0] = opcode_token(OPCODE_BFI, bfi.len() as u32);
    tokens.extend_from_slice(&bfi);

    // ubfe r5.xyzw, r1.xyzw, r2.xyzw, r3.xyzw
    let mut ubfe = vec![opcode_token(OPCODE_UBFE, 0)];
    ubfe.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    ubfe.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    ubfe.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    ubfe.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    ubfe[0] = opcode_token(OPCODE_UBFE, ubfe.len() as u32);
    tokens.extend_from_slice(&ubfe);

    // ibfe r6.xyzw, r1.xyzw, r2.xyzw, r3.xyzw
    let mut ibfe = vec![opcode_token(OPCODE_IBFE, 0)];
    ibfe.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    ibfe.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 1));
    ibfe.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    ibfe.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    ibfe[0] = opcode_token(OPCODE_IBFE, ibfe.len() as u32);
    tokens.extend_from_slice(&ibfe);

    // bfrev r7.xyzw, r3.xyzw
    let mut bfrev = vec![opcode_token(OPCODE_BFREV, 0)];
    bfrev.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 7, WriteMask::XYZW));
    bfrev.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    bfrev[0] = opcode_token(OPCODE_BFREV, bfrev.len() as u32);
    tokens.extend_from_slice(&bfrev);

    // countbits r8.xyzw, r3.xyzw
    let mut countbits = vec![opcode_token(OPCODE_COUNTBITS, 0)];
    countbits.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 8, WriteMask::XYZW));
    countbits.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    countbits[0] = opcode_token(OPCODE_COUNTBITS, countbits.len() as u32);
    tokens.extend_from_slice(&countbits);

    // firstbit_hi r9.xyzw, r3.xyzw
    let mut firstbit_hi = vec![opcode_token(OPCODE_FIRSTBIT_HI, 0)];
    firstbit_hi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 9, WriteMask::XYZW));
    firstbit_hi.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    firstbit_hi[0] = opcode_token(OPCODE_FIRSTBIT_HI, firstbit_hi.len() as u32);
    tokens.extend_from_slice(&firstbit_hi);

    // firstbit_lo r10.xyzw, r3.xyzw
    let mut firstbit_lo = vec![opcode_token(OPCODE_FIRSTBIT_LO, 0)];
    firstbit_lo.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 10, WriteMask::XYZW));
    firstbit_lo.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    firstbit_lo[0] = opcode_token(OPCODE_FIRSTBIT_LO, firstbit_lo.len() as u32);
    tokens.extend_from_slice(&firstbit_lo);

    // firstbit_shi r11.xyzw, r3.xyzw
    let mut firstbit_shi = vec![opcode_token(OPCODE_FIRSTBIT_SHI, 0)];
    firstbit_shi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 11, WriteMask::XYZW));
    firstbit_shi.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    firstbit_shi[0] = opcode_token(OPCODE_FIRSTBIT_SHI, firstbit_shi.len() as u32);
    tokens.extend_from_slice(&firstbit_shi);

    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("insertBits("),
        "expected bfi lowering to use insertBits():\n{wgsl}"
    );
    assert!(
        wgsl.contains("extractBits("),
        "expected bfe lowering to use extractBits():\n{wgsl}"
    );
    assert!(
        wgsl.contains("reverseBits("),
        "expected bfrev lowering to use reverseBits():\n{wgsl}"
    );
    assert!(
        wgsl.contains("countOneBits("),
        "expected countbits lowering to use countOneBits():\n{wgsl}"
    );
    assert!(
        wgsl.contains("firstLeadingBit("),
        "expected firstbit lowering to use firstLeadingBit():\n{wgsl}"
    );
    assert!(
        wgsl.contains("firstTrailingBit("),
        "expected firstbit_lo lowering to use firstTrailingBit():\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_addc_subc_ops_translate_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // iaddc r0.xyzw, r1.xyzw, r2.xyzw, r3.xyzw
    let mut iaddc = vec![opcode_token(OPCODE_IADDC, 0)];
    iaddc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    iaddc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    iaddc.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    iaddc.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    iaddc[0] = opcode_token(OPCODE_IADDC, iaddc.len() as u32);
    tokens.extend_from_slice(&iaddc);

    // uaddc r4.xyzw, r5.xyzw, r2.xyzw, r3.xyzw
    let mut uaddc = vec![opcode_token(OPCODE_UADDC, 0)];
    uaddc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    uaddc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    uaddc.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    uaddc.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    uaddc[0] = opcode_token(OPCODE_UADDC, uaddc.len() as u32);
    tokens.extend_from_slice(&uaddc);

    // isubc r6.xyzw, r7.xyzw, r2.xyzw, r3.xyzw
    let mut isubc = vec![opcode_token(OPCODE_ISUBC, 0)];
    isubc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    isubc.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 7, WriteMask::XYZW));
    isubc.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    isubc.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    isubc[0] = opcode_token(OPCODE_ISUBC, isubc.len() as u32);
    tokens.extend_from_slice(&isubc);

    // usubb r8.xyzw, r9.xyzw, r2.xyzw, r3.xyzw
    let mut usubb = vec![opcode_token(OPCODE_USUBB, 0)];
    usubb.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 8, WriteMask::XYZW));
    usubb.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 9, WriteMask::XYZW));
    usubb.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    usubb.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    usubb[0] = opcode_token(OPCODE_USUBB, usubb.len() as u32);
    tokens.extend_from_slice(&usubb);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("select(vec4<u32>(0u), vec4<u32>(1u)"),
        "expected add/sub-with-carry lowering to use select(..., vec4<u32>(1u), ...):\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_int_mul_hi_ops_translate_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // umul r0.xyzw, r1.xyzw, r2.xyzw, r3.xyzw
    let mut umul = vec![opcode_token(OPCODE_UMUL, 0)];
    umul.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    umul.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    umul.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    umul.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    umul[0] = opcode_token(OPCODE_UMUL, umul.len() as u32);
    tokens.extend_from_slice(&umul);

    // imul r4.xyzw, r5.xyzw, r2.xyzw, r3.xyzw
    let mut imul = vec![opcode_token(OPCODE_IMUL, 0)];
    imul.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 4, WriteMask::XYZW));
    imul.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 5, WriteMask::XYZW));
    imul.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    imul.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    imul[0] = opcode_token(OPCODE_IMUL, imul.len() as u32);
    tokens.extend_from_slice(&imul);

    // umad r6.xyzw, r7.xyzw, r2.xyzw, r3.xyzw, r8.xyzw
    let mut umad = vec![opcode_token(OPCODE_UMAD, 0)];
    umad.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 6, WriteMask::XYZW));
    umad.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 7, WriteMask::XYZW));
    umad.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    umad.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    umad.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 8));
    umad[0] = opcode_token(OPCODE_UMAD, umad.len() as u32);
    tokens.extend_from_slice(&umad);

    // imad r9.xyzw, r10.xyzw, r2.xyzw, r3.xyzw, r8.xyzw
    let mut imad = vec![opcode_token(OPCODE_IMAD, 0)];
    imad.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 9, WriteMask::XYZW));
    imad.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 10, WriteMask::XYZW));
    imad.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 2));
    imad.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 3));
    imad.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, 8));
    imad[0] = opcode_token(OPCODE_IMAD, imad.len() as u32);
    tokens.extend_from_slice(&imad);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(">> 32u"),
        "expected mul/mad hi-part lowering to shift by 32:\n{wgsl}"
    );
    assert!(
        wgsl.contains("u64(("),
        "expected unsigned hi-part lowering to use u64 intermediates:\n{wgsl}"
    );
    assert!(
        wgsl.contains("i64(("),
        "expected signed hi-part lowering to use i64 intermediates:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}
