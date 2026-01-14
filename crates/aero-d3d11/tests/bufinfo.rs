use aero_d3d11::sm4::decode_program;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, DxbcFile, FourCC, ShaderSignatures, Sm4Inst, Sm4Program,
    WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
// `bufinfo` opcode IDs are not currently modeled explicitly; the decoder recognizes the operand
// pattern structurally. Use an unused opcode ID (< 0x100 so it is treated as an instruction) to
// exercise the fallback path.
const OPCODE_TEST_BUFINFO: u32 = 0xff;

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

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1),
        idx,
    ]
}

fn reg_src_resource(slot: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_RESOURCE, 0, OPERAND_SEL_MASK, 0, 1),
        slot,
    ]
}

fn reg_src_uav(slot: u32) -> Vec<u32> {
    vec![
        operand_token(
            OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
            0,
            OPERAND_SEL_MASK,
            0,
            1,
        ),
        slot,
    ]
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
fn decodes_and_translates_bufinfo_raw_to_array_length() {
    // dcl_thread_group 1, 1, 1
    // bufinfo r0.x, t0
    let mut body = vec![
        opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
        1,
        1,
        1,
        opcode_token(OPCODE_TEST_BUFINFO, 1 + 2 + 2),
    ];
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::X));
    body.extend_from_slice(&reg_src_resource(0));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 = compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("SM4 decode");
    assert!(matches!(&module.instructions[0], Sm4Inst::BufInfoRaw { .. }));

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
        .expect("translate");

    assert!(
        translated.wgsl.contains("@compute"),
        "expected compute entry point:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("arrayLength(&t0.data)"),
        "expected bufinfo to use arrayLength:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("* 4u"),
        "expected bufinfo to convert dwords to bytes:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected bufinfo to store integer bits via bitcast:\n{}",
        translated.wgsl
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn decodes_and_translates_bufinfo_structured_uses_decl_stride() {
    // dcl_thread_group 1, 1, 1
    // dcl_resource_structured t0, stride=16
    let mut body = vec![
        opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
        1,
        1,
        1,
        opcode_token(OPCODE_DCL_RESOURCE_STRUCTURED, 4),
    ];
    body.extend_from_slice(&reg_src_resource(0));
    body.push(16);

    // bufinfo r0.xy, t0
    body.push(opcode_token(OPCODE_TEST_BUFINFO, 1 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask(0b0011)));
    body.extend_from_slice(&reg_src_resource(0));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 = compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("SM4 decode");
    assert!(
        matches!(
            module.instructions[0],
            Sm4Inst::BufInfoStructured {
                stride_bytes: 16,
                ..
            }
        ),
        "expected structured bufinfo to be refined using the declared stride"
    );

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
        .expect("translate");
    assert!(
        translated.wgsl.contains("16u"),
        "expected stride literal in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected structured bufinfo to store integer bits via bitcast:\n{}",
        translated.wgsl
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn decodes_and_translates_bufinfo_raw_uav_to_array_length() {
    // dcl_thread_group 1, 1, 1
    // bufinfo r0.x, u0
    let mut body = vec![
        opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
        1,
        1,
        1,
        opcode_token(OPCODE_TEST_BUFINFO, 1 + 2 + 2),
    ];
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::X));
    body.extend_from_slice(&reg_src_uav(0));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 = compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("SM4 decode");
    assert!(matches!(
        module.instructions[0],
        Sm4Inst::BufInfoRawUav { .. }
    ));

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
        .expect("translate");

    assert!(
        translated.wgsl.contains("arrayLength(&u0.data)"),
        "expected uav bufinfo to use arrayLength:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("* 4u"),
        "expected uav bufinfo to convert dwords to bytes:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected uav bufinfo to store integer bits via bitcast:\n{}",
        translated.wgsl
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn decodes_and_translates_bufinfo_structured_uav_uses_decl_stride() {
    // dcl_thread_group 1, 1, 1
    // dcl_uav_structured u0, stride=16
    let mut body = vec![
        opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
        1,
        1,
        1,
        opcode_token(OPCODE_DCL_UAV_STRUCTURED, 4),
    ];
    body.extend_from_slice(&reg_src_uav(0));
    body.push(16);

    // bufinfo r0.xy, u0
    body.push(opcode_token(OPCODE_TEST_BUFINFO, 1 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask(0b0011)));
    body.extend_from_slice(&reg_src_uav(0));

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 5 = compute shader.
    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("SM4 decode");
    assert!(
        matches!(
            module.instructions[0],
            Sm4Inst::BufInfoStructuredUav {
                stride_bytes: 16,
                ..
            }
        ),
        "expected structured uav bufinfo to be refined using the declared stride"
    );

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
        .expect("translate");
    assert!(
        translated.wgsl.contains("16u"),
        "expected stride literal in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("arrayLength(&u0.data)"),
        "expected uav structured bufinfo to use arrayLength:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected structured uav bufinfo to store integer bits via bitcast:\n{}",
        translated.wgsl
    );
    assert_wgsl_validates(&translated.wgsl);
}
