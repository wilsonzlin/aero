use aero_d3d11::sm4::{decode_program, opcode::*};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, CmpOp, CmpType, DxbcFile, DxbcSignature,
    DxbcSignatureParameter, FourCC, OperandModifier, RegFile, RegisterRef, ShaderModel,
    ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, Sm4Program, Sm4TestBool, SrcKind,
    SrcOperand, Swizzle, TextureRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn sig_param(name: &str, index: u32, register: u32, mask: u8) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type: 0,
        component_type: 0,
        register,
        mask,
        read_write_mask: mask,
        stream: 0,
        min_precision: 0,
    }
}

fn build_signature_chunk(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name.as_str(),
            semantic_index: p.semantic_index,
            system_value_type: p.system_value_type,
            component_type: p.component_type,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.read_write_mask,
            stream: u32::from(p.stream),
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    let version = ((stage_type as u32) << 16) | (5u32 << 4);
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn opcode_token_with_test(opcode: u32, len: u32, test: u32) -> u32 {
    opcode
        | (len << OPCODE_LEN_SHIFT)
        | ((test & OPCODE_TEST_BOOLEAN_MASK) << OPCODE_TEST_BOOLEAN_SHIFT)
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

fn assert_wgsl_parses(wgsl: &str) {
    naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
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

#[test]
fn translates_signature_driven_vs_with_empty_input_signature_without_empty_struct() {
    // WGSL forbids empty structs. DXBC vertex shaders can have an empty input signature (e.g. when
    // generating positions procedurally), so ensure we emit `fn vs_main()` rather than
    // `struct VsIn {}` + `fn vs_main(input: VsIn)`.
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, vec![0u8; 8])]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");

    let module = Sm4Module {
        stage: ShaderStage::Vertex,
        model: ShaderModel { major: 4, minor: 0 },
        decls: Vec::new(),
        instructions: vec![Sm4Inst::Ret],
    };
    let signatures = ShaderSignatures {
        isgn: Some(DxbcSignature {
            parameters: Vec::new(),
        }),
        osgn: Some(DxbcSignature {
            parameters: vec![sig_param("SV_Position", 0, 0, 0b1111)],
        }),
        psgn: None,
        pcsg: None,
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .expect("translation should succeed");
    assert!(
        !translated.wgsl.contains("struct VsIn {"),
        "expected VS translation to omit VsIn struct when it would be empty:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("fn vs_main() -> VsOut"),
        "expected VS entry point to take no parameters:\n{}",
        translated.wgsl
    );
    assert_wgsl_parses(&translated.wgsl);
}

#[test]
fn translates_packed_signature_params_by_merging_masks() {
    // DXBC signatures can pack multiple semantics into a single register (e.g. TEXCOORD0.xy and
    // TEXCOORD1.zw both mapped to register 1). The translator should treat the register as a full
    // vec4 rather than clobbering components based on whichever signature parameter it sees last.
    let osgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011), // xy
        sig_param("TEXCOORD", 1, 1, 0b1100), // zw
    ];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let imm = |x: f32, y: f32, z: f32, w: f32| SrcOperand {
        kind: SrcKind::ImmediateF32([x.to_bits(), y.to_bits(), z.to_bits(), w.to_bits()]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let dst_out = |idx: u32, mask: u8| aero_d3d11::DstOperand {
        reg: RegisterRef {
            file: RegFile::Output,
            index: idx,
        },
        mask: WriteMask(mask),
        saturate: false,
    };

    let module = Sm4Module {
        stage: ShaderStage::Vertex,
        model: ShaderModel { major: 4, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            // o0 = vec4(0,0,0,1) (position)
            Sm4Inst::Mov {
                dst: dst_out(0, 0b1111),
                src: imm(0.0, 0.0, 0.0, 1.0),
            },
            // o1.xy = (1,0)
            Sm4Inst::Mov {
                dst: dst_out(1, 0b0011),
                src: imm(1.0, 0.0, 0.0, 0.0),
            },
            // o1.zw = (1,1)
            Sm4Inst::Mov {
                dst: dst_out(1, 0b1100),
                src: imm(0.0, 0.0, 1.0, 1.0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);

    // If the packed masks are not merged, the translator would emit a default-fill expression such
    // as `out.o1 = vec4<f32>(0.0, 0.0, o1.z, o1.w);` (dropping xy) or `out.o1 = vec4<f32>(o1.x,
    // o1.y, 0.0, 1.0);` (dropping zw). After merging masks, we should preserve the full register.
    assert!(
        translated.wgsl.contains("out.o1 = o1;"),
        "expected packed register assignment to preserve all components:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("out.o1 = vec4<f32>"),
        "unexpected default-fill applied to packed register:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_sample_shader_from_dxbc() {
    const DCL_INPUT: u32 = 0x100;
    const DCL_OUTPUT: u32 = 0x101;
    const DCL_RESOURCE: u32 = 0x102;
    const DCL_SAMPLER: u32 = 0x103;

    let mut body = Vec::<u32>::new();

    // dcl_input v0.xy
    body.push(opcode_token(DCL_INPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask(0b0011)));
    // dcl_output o0.xyzw
    body.push(opcode_token(DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    // dcl_resource_texture2d t0 (dimension token ignored by our decl decoder)
    let tex_decl = reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW);
    body.push(opcode_token(
        DCL_RESOURCE,
        1 + tex_decl.len() as u32 + 1, /* + dimension token */
    ));
    body.extend_from_slice(&tex_decl);
    body.push(2);
    // dcl_sampler s0
    let samp_decl = reg_src(OPERAND_TYPE_SAMPLER, &[0], Swizzle::XYZW);
    body.push(opcode_token(DCL_SAMPLER, 1 + samp_decl.len() as u32));
    body.extend_from_slice(&samp_decl);

    // sample r0, v0, t0, s0
    body.push(opcode_token(OPCODE_SAMPLE, 1 + 2 + 2 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_SAMPLER, &[0], Swizzle::XYZW));

    // mov o0, r0
    body.push(opcode_token(OPCODE_MOV, 5));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (
            FOURCC_ISGN,
            build_signature_chunk(&[sig_param("TEXCOORD", 0, 0, 0b0011)]),
        ),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Pixel);

    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(module.instructions.len(), 3);
    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::Input { .. })));
    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::Output { .. })));
    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::ResourceTexture2D { slot: 0 })));
    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::Sampler { slot: 0 })));

    // Spot-check that sample operands decoded as expected.
    assert_eq!(
        module.instructions[0],
        Sm4Inst::Sample {
            dst: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            coord: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Input,
                    index: 0
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            texture: TextureRef { slot: 0 },
            sampler: aero_d3d11::SamplerRef { slot: 0 },
        }
    );

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains("@fragment"));
    assert!(translated.wgsl.contains("textureSample(t0, s0"));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, aero_d3d11::BindingKind::Texture2D { slot: 0 })));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, aero_d3d11::BindingKind::Sampler { slot: 0 })));
}

#[test]
fn decodes_and_translates_ld_shader_from_dxbc() {
    const DCL_INPUT: u32 = 0x100;
    const DCL_OUTPUT: u32 = 0x101;
    const DCL_RESOURCE: u32 = 0x102;

    let mut body = Vec::<u32>::new();

    // dcl_input v0.xy
    body.push(opcode_token(DCL_INPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask(0b0011)));
    // dcl_output o0.xyzw
    body.push(opcode_token(DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    // dcl_resource_texture2d t0 (dimension token ignored by our decl decoder)
    let tex_decl = reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW);
    body.push(opcode_token(
        DCL_RESOURCE,
        1 + tex_decl.len() as u32 + 1, /* + dimension token */
    ));
    body.extend_from_slice(&tex_decl);
    body.push(2);

    // ld r0, v0, t0
    body.push(opcode_token(OPCODE_LD, 1 + 2 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW));

    // mov o0, r0
    body.push(opcode_token(OPCODE_MOV, 5));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (
            FOURCC_ISGN,
            build_signature_chunk(&[sig_param("TEXCOORD", 0, 0, 0b0011)]),
        ),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Pixel);

    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(module.instructions.len(), 3);
    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::ResourceTexture2D { slot: 0 })));

    // Spot-check that ld operands decoded as expected.
    assert_eq!(
        module.instructions[0],
        Sm4Inst::Ld {
            dst: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            coord: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Input,
                    index: 0
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
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

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains("@fragment"));
    assert!(translated.wgsl.contains("textureLoad(t0"));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, aero_d3d11::BindingKind::Texture2D { slot: 0 })));
    assert!(!translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, aero_d3d11::BindingKind::Sampler { .. })));
}

#[test]
fn decodes_and_translates_minimal_compute_shader_without_signatures() {
    // Minimal compute shader token stream: `dcl_thread_group` + `ret`.
    let body = vec![
        opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
        8,
        4,
        1,
        opcode_token(OPCODE_RET, 1),
    ];

    // Stage type 5 = compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    // Compute shaders frequently omit ISGN/OSGN signature chunks; decoding should still succeed.
    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(module.stage, ShaderStage::Compute);
    assert_eq!(module.instructions, vec![Sm4Inst::Ret]);
    assert!(module
        .decls
        .iter()
        .any(|d| matches!(d, Sm4Decl::ThreadGroupSize { x: 8, y: 4, z: 1 })));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    assert!(signatures.isgn.is_none());
    assert!(signatures.osgn.is_none());

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("@workgroup_size(8, 4, 1)"));
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn decodes_and_translates_compute_shader_with_srv_and_uav_buffers() {
    // Minimal compute shader token stream:
    // - dcl_thread_group 8,1,1
    // - dcl_resource_raw t0
    // - dcl_uav_raw u0
    // - ld_raw r0.x, l(0), t0
    // - store_raw u0.x, l(0), r0.x
    // - ret
    let mut body = Vec::<u32>::new();

    // dcl_thread_group 8,1,1
    body.push(opcode_token(OPCODE_DCL_THREAD_GROUP, 4));
    body.push(8);
    body.push(1);
    body.push(1);

    // dcl_resource_raw t0
    body.push(opcode_token(OPCODE_DCL_RESOURCE_RAW, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_RESOURCE, 0, WriteMask::XYZW));

    // dcl_uav_raw u0
    body.push(opcode_token(OPCODE_DCL_UAV_RAW, 3));
    body.extend_from_slice(&reg_dst(
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        0,
        WriteMask::XYZW,
    ));

    // ld_raw r0.x, l(0), t0
    let addr0 = imm32_scalar(0);
    let t0 = reg_src(OPERAND_TYPE_RESOURCE, &[0], Swizzle::XYZW);
    body.push(opcode_token(
        OPCODE_LD_RAW,
        1 + 2 + addr0.len() as u32 + t0.len() as u32,
    ));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::X));
    body.extend_from_slice(&addr0);
    body.extend_from_slice(&t0);

    // store_raw u0.x, l(0), r0.x
    let r0 = reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XXXX);
    body.push(opcode_token(
        OPCODE_STORE_RAW,
        1 + 2 + addr0.len() as u32 + r0.len() as u32,
    ));
    body.extend_from_slice(&reg_dst(
        OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
        0,
        WriteMask::X,
    ));
    body.extend_from_slice(&addr0);
    body.extend_from_slice(&r0);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 = compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("SM4 decode");
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::LdRaw { .. })));
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::StoreRaw { .. })));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("@workgroup_size(8, 1, 1)"));
    assert!(translated
        .wgsl
        .contains("@group(2) @binding(32) var<storage, read> t0: AeroStorageBufferU32;"));
    assert!(translated
        .wgsl
        .contains("@group(2) @binding(176) var<storage, read_write> u0: AeroStorageBufferU32;"));

    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, aero_d3d11::BindingKind::SrvBuffer { slot: 0 })));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, aero_d3d11::BindingKind::UavBuffer { slot: 0 })));
}

#[test]
fn decodes_and_translates_switch_shader_from_dxbc() {
    const DCL_INPUT: u32 = 0x100;
    const DCL_OUTPUT: u32 = 0x101;

    let mut body = Vec::<u32>::new();

    // dcl_input v0.x
    body.push(opcode_token(DCL_INPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::X));
    // dcl_output o0.xyzw
    body.push(opcode_token(DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));

    // switch v0.x
    let selector = reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XXXX);
    body.push(opcode_token(OPCODE_SWITCH, 1 + selector.len() as u32));
    body.extend_from_slice(&selector);

    // case 0:
    let case0 = imm32_scalar(0);
    body.push(opcode_token(OPCODE_CASE, 1 + case0.len() as u32));
    body.extend_from_slice(&case0);
    // mov o0, vec4(1,0,0,1)
    let mov0_imm = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + mov0_imm.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&mov0_imm);
    body.push(opcode_token(OPCODE_BREAK, 1));

    // case 1:
    let case1 = imm32_scalar(1);
    body.push(opcode_token(OPCODE_CASE, 1 + case1.len() as u32));
    body.extend_from_slice(&case1);
    // mov o0, vec4(0,1,0,1)
    let mov1_imm = imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + mov1_imm.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&mov1_imm);
    body.push(opcode_token(OPCODE_BREAK, 1));

    // default:
    body.push(opcode_token(OPCODE_DEFAULT, 1));
    // mov o0, vec4(0,0,1,1)
    let movd_imm = imm32_vec4([
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + movd_imm.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&movd_imm);
    body.push(opcode_token(OPCODE_BREAK, 1));

    body.push(opcode_token(OPCODE_ENDSWITCH, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (
            FOURCC_ISGN,
            build_signature_chunk(&[sig_param("TEXCOORD", 0, 0, 0b0001)]),
        ),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Pixel);

    let module = decode_program(&program).expect("SM4 decode");
    assert!(matches!(
        module.instructions.first(),
        Some(Sm4Inst::Switch { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let switch_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("switch("))
        .expect("expected a switch statement");
    assert!(
        switch_line.contains("bitcast<vec4<i32>>"),
        "expected switch selector to be derived from raw integer bits:\n{switch_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    assert!(
        !switch_line.contains("floor(")
            && !switch_line.contains("select(")
            && !switch_line.contains("i32("),
        "switch lowering should not use float-vs-bitcast heuristics:\n{switch_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    assert!(translated.wgsl.contains("case 0i"));
    assert!(translated.wgsl.contains("default:"));
}

#[test]
fn switch_groups_consecutive_case_labels() {
    const DCL_INPUT: u32 = 0x100;
    const DCL_OUTPUT: u32 = 0x101;

    let mut body = Vec::<u32>::new();

    // dcl_input v0.x
    body.push(opcode_token(DCL_INPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::X));
    // dcl_output o0.xyzw
    body.push(opcode_token(DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));

    // switch v0.x
    let selector = reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XXXX);
    body.push(opcode_token(OPCODE_SWITCH, 1 + selector.len() as u32));
    body.extend_from_slice(&selector);

    // case 0:
    let case0 = imm32_scalar(0);
    body.push(opcode_token(OPCODE_CASE, 1 + case0.len() as u32));
    body.extend_from_slice(&case0);
    // case 1:
    let case1 = imm32_scalar(1);
    body.push(opcode_token(OPCODE_CASE, 1 + case1.len() as u32));
    body.extend_from_slice(&case1);

    // mov o0, vec4(1,0,0,1)
    let red = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + red.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&red);
    body.push(opcode_token(OPCODE_BREAK, 1));

    // default:
    body.push(opcode_token(OPCODE_DEFAULT, 1));
    // mov o0, vec4(0,0,1,1)
    let blue = imm32_vec4([
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + blue.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&blue);
    body.push(opcode_token(OPCODE_BREAK, 1));

    body.push(opcode_token(OPCODE_ENDSWITCH, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (
            FOURCC_ISGN,
            build_signature_chunk(&[sig_param("TEXCOORD", 0, 0, 0b0001)]),
        ),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("case 0i, 1i"),
        "expected grouped WGSL case labels:\n{}",
        translated.wgsl
    );
}

#[test]
fn switch_falls_through_when_break_is_omitted() {
    const DCL_INPUT: u32 = 0x100;
    const DCL_OUTPUT: u32 = 0x101;

    let mut body = Vec::<u32>::new();

    // dcl_input v0.x
    body.push(opcode_token(DCL_INPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::X));
    // dcl_output o0.xyzw
    body.push(opcode_token(DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));

    // switch v0.x
    let selector = reg_src(OPERAND_TYPE_INPUT, &[0], Swizzle::XXXX);
    body.push(opcode_token(OPCODE_SWITCH, 1 + selector.len() as u32));
    body.extend_from_slice(&selector);

    // case 0:
    let case0 = imm32_scalar(0);
    body.push(opcode_token(OPCODE_CASE, 1 + case0.len() as u32));
    body.extend_from_slice(&case0);
    // mov o0, vec4(1,0,0,1)
    let red = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + red.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&red);
    // no break; should fall through

    // case 1:
    let case1 = imm32_scalar(1);
    body.push(opcode_token(OPCODE_CASE, 1 + case1.len() as u32));
    body.extend_from_slice(&case1);
    // mov o0, vec4(0,1,0,1)
    let green = imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + green.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&green);
    body.push(opcode_token(OPCODE_BREAK, 1));

    body.push(opcode_token(OPCODE_ENDSWITCH, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (
            FOURCC_ISGN,
            build_signature_chunk(&[sig_param("TEXCOORD", 0, 0, 0b0001)]),
        ),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let wgsl = &translated.wgsl;
    assert!(
        !wgsl.contains("fallthrough;"),
        "WGSL should not require an explicit fallthrough statement:\n{wgsl}"
    );

    let idx_case0 = wgsl.find("case 0i").expect("case 0");
    let idx_case1 = wgsl.find("case 1i").expect("case 1");
    assert!(
        !wgsl[idx_case0..idx_case1].contains("break;"),
        "expected case 0 to fall through to case 1 (no `break;` between labels):\n{wgsl}"
    );
}

#[test]
fn decodes_and_translates_if_else_endif() {
    let mut body = Vec::<u32>::new();

    // if_nz l(1.0)
    let cond = imm32_scalar(1.0f32.to_bits());
    body.push(
        OPCODE_IF
            | ((1 + cond.len() as u32) << OPCODE_LEN_SHIFT)
            | (1 << OPCODE_TEST_BOOLEAN_SHIFT),
    );
    body.extend_from_slice(&cond);

    // mov o0, l(1,0,0,1)
    let red = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + red.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&red);

    // else
    body.push(opcode_token(OPCODE_ELSE, 1));

    // mov o0, l(0,1,0,1)
    let green = imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + green.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&green);

    // endif
    body.push(opcode_token(OPCODE_ENDIF, 1));

    // ret
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");

    assert!(
        module
            .instructions
            .iter()
            .any(|i| matches!(i, Sm4Inst::If { .. })),
        "expected IF instruction in decoded module: {:#?}",
        module.instructions
    );
    assert!(module.instructions.iter().all(|i| {
        !matches!(
            i,
            Sm4Inst::Unknown { opcode }
                if *opcode == OPCODE_IF || *opcode == OPCODE_ELSE || *opcode == OPCODE_ENDIF
        )
    }));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("if ("));
    assert!(translated.wgsl.contains("} else {"));
    assert!(
        translated.wgsl.contains("}"),
        "expected closing braces in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_uaddc_shader_from_dxbc() {
    let mut body = Vec::<u32>::new();

    // uaddc r0, r1, r2, r3
    body.push(opcode_token(OPCODE_UADDC, 1 + 2 + 2 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[2], Swizzle::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[3], Swizzle::XYZW));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Pixel);

    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(module.instructions.len(), 2);

    assert_eq!(
        module.instructions[0],
        Sm4Inst::UAddC {
            dst_sum: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            dst_carry: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 1
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            a: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 2
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            b: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 3
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }
    );

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(
        translated.wgsl.contains("let uaddc_carry_0"),
        "expected uaddc carry logic in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_isubc_shader_from_dxbc() {
    let mut body = Vec::<u32>::new();

    // isubc r0, r1, r2, r3
    body.push(opcode_token(OPCODE_ISUBC, 1 + 2 + 2 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[2], Swizzle::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[3], Swizzle::XYZW));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Pixel);

    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(module.instructions.len(), 2);

    assert_eq!(
        module.instructions[0],
        Sm4Inst::ISubC {
            dst_diff: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 0
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            dst_carry: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Temp,
                    index: 1
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            a: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 2
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
            b: SrcOperand {
                kind: SrcKind::Register(RegisterRef {
                    file: RegFile::Temp,
                    index: 3
                }),
                swizzle: Swizzle::XYZW,
                modifier: OperandModifier::None,
            },
        }
    );

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(
        translated.wgsl.contains("let isubc_carry_0"),
        "expected isubc carry logic in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn ret_inside_if_does_not_break_brace_balancing() {
    let mut body = Vec::<u32>::new();

    // if_nz l(0.0) (false at runtime, but exercises codegen)
    let cond = imm32_scalar(0.0f32.to_bits());
    body.push(
        OPCODE_IF
            | ((1 + cond.len() as u32) << OPCODE_LEN_SHIFT)
            | (1 << OPCODE_TEST_BOOLEAN_SHIFT),
    );
    body.extend_from_slice(&cond);

    // mov o0, l(1,0,0,1)
    let red = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + red.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&red);

    // ret (inside if)
    body.push(opcode_token(OPCODE_RET, 1));

    // endif
    body.push(opcode_token(OPCODE_ENDIF, 1));

    // mov o0, l(0,1,0,1)
    let green = imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + green.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&green);

    // ret (top-level)
    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Ret)));
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::If { .. })));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("if ("));
    assert!(translated.wgsl.contains("return "));
    assert!(
        translated.wgsl.matches('{').count() == translated.wgsl.matches('}').count(),
        "expected balanced braces in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_compute_thread_group_size_decl_to_workgroup_size() {
    // Minimal compute shader with an explicit thread-group size declaration.
    //
    // dcl_thread_group 8, 8, 1
    // ret
    let body = vec![
        opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
        8,
        8,
        1,
        opcode_token(OPCODE_RET, 1),
    ];

    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(module.stage, ShaderStage::Compute);

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(
        translated.wgsl.contains("@workgroup_size(8, 8, 1)"),
        "expected dcl_thread_group(8,8,1) to map to WGSL workgroup_size:\n{}",
        translated.wgsl
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn ret_inside_if_with_depth_output_validates() {
    let mut body = Vec::<u32>::new();

    // if_nz l(1.0)
    let cond = imm32_scalar(1.0f32.to_bits());
    body.push(
        OPCODE_IF
            | ((1 + cond.len() as u32) << OPCODE_LEN_SHIFT)
            | (1 << OPCODE_TEST_BOOLEAN_SHIFT),
    );
    body.extend_from_slice(&cond);

    // mov o0, l(1,0,0,1)
    let red = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + red.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&red);

    // mov o1.x, l(0.5)
    let depth_a = imm32_scalar(0.5f32.to_bits());
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + depth_a.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::X));
    body.extend_from_slice(&depth_a);

    // ret (inside if)
    body.push(opcode_token(OPCODE_RET, 1));

    // endif
    body.push(opcode_token(OPCODE_ENDIF, 1));

    // mov o0, l(0,1,0,1)
    let green = imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + green.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&green);

    // mov o1.x, l(0.25)
    let depth_b = imm32_scalar(0.25f32.to_bits());
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + depth_b.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::X));
    body.extend_from_slice(&depth_b);

    // ret (top-level)
    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[
                sig_param("SV_Target", 0, 0, 0b1111),
                sig_param("SV_Depth", 0, 1, 0b0001),
            ]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::If { .. })));
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Ret)));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(translated.wgsl.contains("struct PsOut"));
    assert!(translated.wgsl.contains("@builtin(frag_depth)"));
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.matches("return").count() >= 2,
        "expected both early and epilogue returns:\n{}",
        translated.wgsl
    );
}

#[test]
fn compute_ret_inside_if_validates() {
    // Minimal compute shader: `dcl_thread_group` + `if_nz` + `ret` + `endif` + `ret`.
    let mut body = Vec::<u32>::new();
    body.push(opcode_token(OPCODE_DCL_THREAD_GROUP, 4));
    body.extend_from_slice(&[8, 4, 1]);

    // if_nz l(1.0)
    let cond = imm32_scalar(1.0f32.to_bits());
    body.push(
        OPCODE_IF
            | ((1 + cond.len() as u32) << OPCODE_LEN_SHIFT)
            | (1 << OPCODE_TEST_BOOLEAN_SHIFT),
    );
    body.extend_from_slice(&cond);

    body.push(opcode_token(OPCODE_RET, 1));
    body.push(opcode_token(OPCODE_ENDIF, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("if ("));
    assert!(translated.wgsl.contains("return;"));
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn decodes_and_translates_if_z_else_endif_vertex_shader() {
    let mut body = Vec::<u32>::new();

    // if_z l(0.0)
    let cond = imm32_scalar(0.0f32.to_bits());
    body.push(OPCODE_IF | ((1 + cond.len() as u32) << OPCODE_LEN_SHIFT));
    body.extend_from_slice(&cond);

    // mov o0, l(0,0,0,1)
    let pos_a = imm32_vec4([
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + pos_a.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&pos_a);

    // else
    body.push(opcode_token(OPCODE_ELSE, 1));

    // mov o0, l(1,0,0,1)
    let pos_b = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + pos_b.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&pos_b);

    // endif + ret
    body.push(opcode_token(OPCODE_ENDIF, 1));
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 1 = vertex shader.
    let tokens = make_sm5_program_tokens(1, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Position", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Vertex);
    let module = decode_program(&program).expect("SM4 decode");

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@vertex"));
    assert!(translated.wgsl.contains("if ("));
    assert!(translated.wgsl.contains("} else {"));
    assert!(
        translated.wgsl.contains("bitcast<u32>") && translated.wgsl.contains("== 0u"),
        "expected if_z lowering to compare raw 32-bit bits against zero (bitcast<u32>(...) == 0u):\n{}",
        translated.wgsl
    );
}

#[test]
fn vertex_ret_inside_if_does_not_break_brace_balancing() {
    let mut body = Vec::<u32>::new();

    // if_nz l(0.0) (false at runtime, but exercises codegen)
    let cond = imm32_scalar(0.0f32.to_bits());
    body.push(
        OPCODE_IF
            | ((1 + cond.len() as u32) << OPCODE_LEN_SHIFT)
            | (1 << OPCODE_TEST_BOOLEAN_SHIFT),
    );
    body.extend_from_slice(&cond);

    // mov o0, l(0,0,0,1)
    let pos_a = imm32_vec4([
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + pos_a.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&pos_a);

    // ret (inside if)
    body.push(opcode_token(OPCODE_RET, 1));

    // endif
    body.push(opcode_token(OPCODE_ENDIF, 1));

    // mov o0, l(1,0,0,1)
    let pos_b = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + pos_b.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&pos_b);

    // ret (top-level)
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 1 = vertex shader.
    let tokens = make_sm5_program_tokens(1, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Position", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.matches('{').count() == translated.wgsl.matches('}').count(),
        "expected balanced braces in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.matches("return out;").count() >= 2,
        "expected both early and epilogue returns:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_depth_output_via_output_depth_operand() {
    // Minimal ps_5_0:
    //   mov oDepth.x, l(0.25)
    //   ret
    //
    // The `oDepth` operand is encoded using `D3D10_SB_OPERAND_TYPE_OUTPUT_DEPTH` and does not
    // necessarily contain a concrete `o#` index; it must be mapped via the output signature's
    // `SV_Depth` register.
    let mut body = Vec::<u32>::new();

    let imm = imm32_vec4([0.25f32.to_bits(); 4]);
    body.push(opcode_token(OPCODE_MOV, (1 + 1 + imm.len()) as u32));
    body.push(operand_token(
        OPERAND_TYPE_OUTPUT_DEPTH,
        2,
        OPERAND_SEL_MASK,
        WriteMask::X.0 as u32,
        0,
        false,
    ));
    body.extend_from_slice(&imm);
    body.push(opcode_token(OPCODE_RET, 1));
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            // Use an unusual register index to ensure the translator is actually using the
            // signature mapping rather than the (missing) operand index.
            build_signature_chunk(&[sig_param("SV_Depth", 0, 5, 0b0001)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

    assert!(
        translated.wgsl.contains("@builtin(frag_depth)"),
        "expected pixel depth output in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("var o5: vec4<f32>"),
        "{}",
        translated.wgsl
    );
    assert!(translated.wgsl.contains("o5.x"), "{}", translated.wgsl);
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn decodes_and_translates_depth_output_via_output_depth_operand_with_overlapping_signature_registers(
) {
    // Minimal ps_5_0:
    //   mov o0, l(1,0,0,1)
    //   mov oDepth.x, l(0.25)
    //   ret
    //
    // DXBC signatures can assign `SV_Target0` and `SV_Depth` the same register number since they
    // live in different register files. Ensure we still translate the `OUTPUT_DEPTH` operand into
    // `@builtin(frag_depth)` without colliding with `o0`.
    let mut body = Vec::<u32>::new();

    // mov o0, l(1,0,0,1)
    let red = imm32_vec4([
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]);
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + red.len()) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&red);

    // mov oDepth.x, l(0.25)
    let imm = imm32_vec4([0.25f32.to_bits(); 4]);
    body.push(opcode_token(OPCODE_MOV, (1 + 1 + imm.len()) as u32));
    body.push(operand_token(
        OPERAND_TYPE_OUTPUT_DEPTH,
        2,
        OPERAND_SEL_MASK,
        WriteMask::X.0 as u32,
        0,
        false,
    ));
    body.extend_from_slice(&imm);

    body.push(opcode_token(OPCODE_RET, 1));
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[
                sig_param("SV_Target", 0, 0, 0b1111),
                sig_param("SV_Depth", 0, 0, 0b0001),
            ]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

    assert!(
        translated.wgsl.contains("@builtin(frag_depth)"),
        "expected pixel depth output in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("var oDepth: vec4<f32>"),
        "expected dedicated depth register when signature registers overlap:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("out.depth = (oDepth).x"),
        "expected depth return sourced from oDepth:\n{}",
        translated.wgsl
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn decodes_and_translates_ult_shader_from_dxbc() {
    // No declarations needed for this minimal shader; the signature drives IO.
    let mut body = Vec::<u32>::new();

    // ult o0, l(1), l(2)
    body.push(opcode_token(OPCODE_ULT, 1 + 2 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm32_scalar(1));
    body.extend_from_slice(&imm32_scalar(2));
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Pixel);

    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(
        module.instructions[0],
        Sm4Inst::Cmp {
            dst: aero_d3d11::DstOperand {
                reg: RegisterRef {
                    file: RegFile::Output,
                    index: 0,
                },
                mask: WriteMask::XYZW,
                saturate: false,
            },
            a: SrcOperand {
                kind: SrcKind::ImmediateF32([1, 1, 1, 1]),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            b: SrcOperand {
                kind: SrcKind::ImmediateF32([2, 2, 2, 2]),
                swizzle: Swizzle::XXXX,
                modifier: OperandModifier::None,
            },
            op: CmpOp::Lt,
            ty: CmpType::U32,
        }
    );

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("select(vec4<u32>(0u), vec4<u32>(0xffffffffu)"));
}

#[test]
fn decodes_and_translates_discard_and_clip_in_pixel_shader() {
    let mut body = Vec::<u32>::new();

    // discard_nz r0.y (note: r0 is never written; the test checks that src-only temps are still
    // declared in WGSL).
    let discard_src = reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::YYYY);
    body.push(opcode_token_with_test(
        OPCODE_DISCARD,
        1 + discard_src.len() as u32,
        1, // non-zero
    ));
    body.extend_from_slice(&discard_src);

    // discard_z r1.z
    let discard_z_src = reg_src(OPERAND_TYPE_TEMP, &[1], Swizzle::ZZZZ);
    body.push(opcode_token_with_test(
        OPCODE_DISCARD,
        1 + discard_z_src.len() as u32,
        0, // zero
    ));
    body.extend_from_slice(&discard_z_src);

    // clip r2.wzyx
    let clip_src = reg_src(OPERAND_TYPE_TEMP, &[2], Swizzle([3, 2, 1, 0]));
    body.push(opcode_token(OPCODE_CLIP, 1 + clip_src.len() as u32));
    body.extend_from_slice(&clip_src);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Pixel);

    let module = decode_program(&program).expect("SM4 decode");
    assert_eq!(module.instructions.len(), 4);
    assert!(matches!(
        module.instructions[0],
        Sm4Inst::Discard {
            test: Sm4TestBool::NonZero,
            ..
        }
    ));
    assert!(matches!(
        module.instructions[1],
        Sm4Inst::Discard {
            test: Sm4TestBool::Zero,
            ..
        }
    ));
    assert!(matches!(module.instructions[2], Sm4Inst::Clip { .. }));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@fragment"));
    assert!(
        translated.wgsl.contains("discard;"),
        "expected WGSL to contain discard statement:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<u32>"),
        "expected discard to use bitcast<u32> for the condition:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("any(("),
        "expected clip to lower to any(vec4 < 0.0):\n{}",
        translated.wgsl
    );
}

#[test]
fn rejects_discard_in_vertex_shader() {
    let mut body = Vec::<u32>::new();

    let discard_src = reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XXXX);
    body.push(opcode_token_with_test(
        OPCODE_DISCARD,
        1 + discard_src.len() as u32,
        1, // non-zero
    ));
    body.extend_from_slice(&discard_src);
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 1 = vertex shader.
    let tokens = make_sm5_program_tokens(1, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Position", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Vertex);

    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    assert!(matches!(
        err,
        aero_d3d11::ShaderTranslateError::UnsupportedInstruction { inst_index: 0, opcode }
            if opcode == "discard_nz"
    ));
}

#[test]
fn rejects_clip_in_vertex_shader() {
    let mut body = Vec::<u32>::new();

    let clip_src = reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW);
    body.push(opcode_token(OPCODE_CLIP, 1 + clip_src.len() as u32));
    body.extend_from_slice(&clip_src);
    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 1 = vertex shader.
    let tokens = make_sm5_program_tokens(1, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Position", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Vertex);

    let module = decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    assert!(matches!(
        err,
        aero_d3d11::ShaderTranslateError::UnsupportedInstruction { inst_index: 0, opcode }
            if opcode == "clip"
    ));
}

#[test]
fn decodes_and_translates_itof_conversion() {
    const DCL_OUTPUT: u32 = 0x101;

    let mut body = Vec::<u32>::new();

    // dcl_output o0.xyzw
    body.push(opcode_token(DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));

    // mov r0, l(1, 2, 3, 4)  (raw integer bits stored in the untyped register file)
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + 1 + 4) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm32_vec4([1, 2, 3, 4]));

    // itof_sat r1, r0
    let len_without_ext = 1u32 + 2 + 2;
    body.extend_from_slice(&opcode_token_with_sat(OPCODE_ITOF, len_without_ext));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));

    // mov o0, r1
    body.push(opcode_token(OPCODE_MOV, 5));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[1], Swizzle::XYZW));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 = pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");

    assert!(
        matches!(module.instructions[1], Sm4Inst::Itof { .. }),
        "expected second instruction to decode as itof: {:#?}",
        module.instructions
    );

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // `itof` should reinterpret lane bits as signed integers and then numeric-cast to f32.
    assert!(
        translated
            .wgsl
            .contains("vec4<f32>(bitcast<vec4<i32>>(r0))"),
        "expected itof to emit vec4<f32>(bitcast<vec4<i32>>(...)):\n{}",
        translated.wgsl
    );
    // Saturate should clamp float results.
    assert!(
        translated
            .wgsl
            .contains("clamp((vec4<f32>(bitcast<vec4<i32>>(r0)))"),
        "expected itof_sat to clamp the float conversion result:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_ftoi_conversion() {
    const DCL_OUTPUT: u32 = 0x101;

    let mut body = Vec::<u32>::new();

    // dcl_output o0.xyzw
    body.push(opcode_token(DCL_OUTPUT, 3));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));

    // mov r0, l(1.5, 2.5, 3.0, -4.0)
    body.push(opcode_token(OPCODE_MOV, (1 + 2 + 1 + 4) as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm32_vec4([
        1.5f32.to_bits(),
        2.5f32.to_bits(),
        3.0f32.to_bits(),
        (-4.0f32).to_bits(),
    ]));

    // ftoi_sat r1, r0 (saturate should be ignored for integer results)
    let len_without_ext = 1u32 + 2 + 2;
    body.extend_from_slice(&opcode_token_with_sat(OPCODE_FTOI, len_without_ext));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XYZW));

    // mov o0, r1
    body.push(opcode_token(OPCODE_MOV, 5));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_TEMP, &[1], Swizzle::XYZW));

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = decode_program(&program).expect("SM4 decode");

    assert!(
        matches!(module.instructions[1], Sm4Inst::Ftoi { .. }),
        "expected second instruction to decode as ftoi: {:#?}",
        module.instructions
    );

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // `ftoi` should numeric-cast to i32 and then bitcast to the f32 register file.
    assert!(
        translated
            .wgsl
            .contains("bitcast<vec4<f32>>(vec4<i32>(r0))"),
        "expected ftoi to emit bitcast<vec4<f32>>(vec4<i32>(...)):\n{}",
        translated.wgsl
    );
    // Saturate should not be applied to integer results.
    assert!(
        !translated.wgsl.contains("clamp(("),
        "did not expect any clamp() calls for ftoi:\n{}",
        translated.wgsl
    );
}
