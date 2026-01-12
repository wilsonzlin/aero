use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, DxbcSignature, DxbcSignatureParameter,
    FourCC, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderSignatures, ShaderStage,
    Sm4Decl, Sm4Inst, Sm4Module, Sm4Program, SrcKind, SrcOperand, Swizzle, TextureRef, WriteMask,
};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }
    let total_size = cursor as u32;

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    for (fourcc, data) in chunks {
        bytes.extend_from_slice(&fourcc.0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }
    bytes
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
    // Same layout as D3D10+ signature chunks:
    // header: u32 param_count, u32 param_offset
    // table entries: 24 bytes each
    let param_count = u32::try_from(params.len()).expect("too many signature params");
    let header_len = 8usize;
    let entry_size = 24usize;
    let table_len = params.len() * entry_size;

    let mut strings = Vec::<u8>::new();
    let mut name_offsets = Vec::<u32>::with_capacity(params.len());
    for p in params {
        name_offsets.push((header_len + table_len + strings.len()) as u32);
        strings.extend_from_slice(p.semantic_name.as_bytes());
        strings.push(0);
    }

    let mut bytes = Vec::with_capacity(header_len + table_len + strings.len());
    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&(header_len as u32).to_le_bytes());

    for (p, &name_off) in params.iter().zip(name_offsets.iter()) {
        bytes.extend_from_slice(&name_off.to_le_bytes());
        bytes.extend_from_slice(&p.semantic_index.to_le_bytes());
        bytes.extend_from_slice(&p.system_value_type.to_le_bytes());
        bytes.extend_from_slice(&p.component_type.to_le_bytes());
        bytes.extend_from_slice(&p.register.to_le_bytes());
        bytes.push(p.mask);
        bytes.push(p.read_write_mask);
        bytes.push(p.stream);
        bytes.push(p.min_precision);
    }
    bytes.extend_from_slice(&strings);
    bytes
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

    let module = program.decode().expect("SM4 decode");
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

    let module = program.decode().expect("SM4 decode");
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
