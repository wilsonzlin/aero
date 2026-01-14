use aero_d3d11::sm4::{decode_program, opcode::*};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, Builtin, DxbcFile, DxbcSignatureParameter,
    FourCC, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Inst, Sm4Module,
    Sm4Program, SrcKind, SrcOperand, Swizzle, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

// `D3D_NAME` system-value IDs (subset).
const D3D_NAME_POSITION: u32 = 1;
const D3D_NAME_VERTEX_ID: u32 = 6;
const D3D_NAME_PRIMITIVE_ID: u32 = 7;
const D3D_NAME_INSTANCE_ID: u32 = 8;

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout: type in bits 16.., major in bits 4..7, minor in bits 0..3.
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

fn build_signature_chunk(params: &[DxbcSignatureParameter]) -> Vec<u8> {
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

fn sig_param(
    name: &str,
    index: u32,
    register: u32,
    mask: u8,
    sys_value: u32,
) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type: sys_value,
        component_type: 0,
        register,
        mask,
        read_write_mask: mask,
        stream: 0,
        min_precision: 0,
    }
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

#[test]
fn translates_vertex_id_and_instance_id_builtins() {
    // Use the real decoder to ensure the declaration path (`dcl_input_siv`) is exercised.
    const DCL_DUMMY: u32 = 0x100;

    let mut body = Vec::<u32>::new();
    // dcl_input_siv v0.x, SV_VertexID
    body.push(opcode_token(DCL_DUMMY, 4));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::X));
    body.push(D3D_NAME_VERTEX_ID);
    // dcl_input_siv v1.x, SV_InstanceID
    body.push(opcode_token(DCL_DUMMY + 1, 4));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 1, WriteMask::X));
    body.push(D3D_NAME_INSTANCE_ID);
    // dcl_output_siv o0.xyzw, SV_Position
    body.push(opcode_token(DCL_DUMMY + 2, 4));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.push(D3D_NAME_POSITION);

    // mov o0, v0
    body.push(opcode_token(OPCODE_MOV, 5));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, 0));
    body.push(opcode_token(OPCODE_RET, 1));

    let shex_bytes = tokens_to_bytes(&make_sm5_program_tokens(1, &body));

    let isgn = build_signature_chunk(&[
        sig_param("VID", 0, 0, 0b0001, 0),
        sig_param("IID", 0, 1, 0b0001, 0),
    ]);
    let osgn = build_signature_chunk(&[sig_param("POS", 0, 0, 0b1111, 0)]);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, shex_bytes),
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse");
    assert_eq!(program.stage, ShaderStage::Vertex);
    let module = decode_program(&program).expect("SM5 decode");

    let signatures = parse_signatures(&dxbc).expect("signature parse");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

    assert_wgsl_validates(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("@builtin(vertex_index) vertex_id: u32"));
    assert!(translated
        .wgsl
        .contains("@builtin(instance_index) instance_id: u32"));
    assert!(translated
        .wgsl
        .contains("@builtin(position) pos: vec4<f32>"));

    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name == "VID" && p.builtin == Some(Builtin::VertexIndex) && p.location.is_none()
    }));
    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name == "IID"
            && p.builtin == Some(Builtin::InstanceIndex)
            && p.location.is_none()
    }));
    assert!(translated.reflection.outputs.iter().any(|p| {
        p.semantic_name == "POS" && p.builtin == Some(Builtin::Position) && p.location.is_none()
    }));
}

#[test]
fn translates_vertex_id_and_instance_id_builtins_from_semantics() {
    // Ensure semantic matching is case-insensitive (D3D semantics are case-insensitive).
    let isgn = build_signature_chunk(&[
        sig_param("SV_VERTEXID", 0, 0, 0b0001, 0),
        sig_param("sv_instanceid", 0, 1, 0b0001, 0),
    ]);
    let osgn = build_signature_chunk(&[sig_param("SV_Position", 0, 0, 0b1111, 0)]);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("signature parse");

    let module = Sm4Module {
        stage: ShaderStage::Vertex,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Add {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                a: src_reg(RegFile::Input, 0),
                b: src_reg(RegFile::Input, 1),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("@builtin(vertex_index) vertex_id: u32"));
    assert!(translated
        .wgsl
        .contains("@builtin(instance_index) instance_id: u32"));
    assert!(translated.wgsl.contains("bitcast<f32>(input.vertex_id)"));
    assert!(translated.wgsl.contains("bitcast<f32>(input.instance_id)"));

    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name.eq_ignore_ascii_case("SV_VertexID")
            && p.builtin == Some(Builtin::VertexIndex)
            && p.location.is_none()
    }));
    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name.eq_ignore_ascii_case("SV_InstanceID")
            && p.builtin == Some(Builtin::InstanceIndex)
            && p.location.is_none()
    }));
}

#[test]
fn translates_vertex_id_and_instance_id_builtins_from_system_value_type() {
    let isgn = build_signature_chunk(&[
        sig_param("VID", 0, 0, 0b0001, D3D_NAME_VERTEX_ID),
        sig_param("IID", 0, 1, 0b0001, D3D_NAME_INSTANCE_ID),
    ]);
    let osgn = build_signature_chunk(&[sig_param("POS", 0, 0, 0b1111, D3D_NAME_POSITION)]);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("signature parse");

    let module = Sm4Module {
        stage: ShaderStage::Vertex,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Add {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                a: src_reg(RegFile::Input, 0),
                b: src_reg(RegFile::Input, 1),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("@builtin(vertex_index) vertex_id: u32"));
    assert!(translated
        .wgsl
        .contains("@builtin(instance_index) instance_id: u32"));

    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name == "VID" && p.builtin == Some(Builtin::VertexIndex) && p.location.is_none()
    }));
    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name == "IID"
            && p.builtin == Some(Builtin::InstanceIndex)
            && p.location.is_none()
    }));
    assert!(translated.reflection.outputs.iter().any(|p| {
        p.semantic_name == "POS" && p.builtin == Some(Builtin::Position) && p.location.is_none()
    }));
}

#[test]
fn translates_front_facing_builtin() {
    let isgn = build_signature_chunk(&[
        sig_param("SV_Position", 0, 0, 0b1111, 0),
        // Explicitly use the canonical uppercase spelling to ensure we accept both
        // `SV_IsFrontFace` and `SV_ISFRONTFACE`.
        sig_param("SV_ISFRONTFACE", 0, 1, 0b0001, 0),
    ]);
    let osgn = build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111, 0)]);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("signature parse");

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("@builtin(front_facing) front_facing: bool"));
    assert!(translated
        .wgsl
        .contains("vec4<f32>(bitcast<f32>(select(0u, 0xffffffffu, input.front_facing)))"));

    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name.eq_ignore_ascii_case("SV_IsFrontFace")
            && p.builtin == Some(Builtin::FrontFacing)
            && p.location.is_none()
    }));
}

#[test]
fn translates_primitive_id_builtin() {
    let isgn = build_signature_chunk(&[
        // Explicit `system_value_type` is set here, but the translator also accepts semantic name
        // matching (`SV_PrimitiveID`).
        sig_param("PID", 0, 0, 0b0001, D3D_NAME_PRIMITIVE_ID),
    ]);
    let osgn = build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111, 0)]);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("signature parse");

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(translated
        .wgsl
        .contains("@builtin(primitive_index) primitive_id: u32"));
    assert!(translated.wgsl.contains("bitcast<f32>(input.primitive_id)"));

    assert!(translated.reflection.inputs.iter().any(|p| {
        p.semantic_name == "PID"
            && p.builtin == Some(Builtin::PrimitiveIndex)
            && p.location.is_none()
    }));
}
