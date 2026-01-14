use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcSignatureParameter, FourCC, Sm4Inst,
    Sm4Program, WriteMask,
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

fn opcode_token_setp(len: u32, cmp: u32) -> u32 {
    OPCODE_SETP | (len << OPCODE_LEN_SHIFT) | (cmp << SETP_CMP_SHIFT)
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

fn reg_src(ty: u32, idx: u32, swizzle: [u8; 4]) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_SWIZZLE, swizzle_bits(swizzle), 1),
        idx,
    ]
}

fn pred_operand(idx: u32, component: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_PREDICATE, 1, OPERAND_SEL_SELECT1, component, 1),
        idx,
    ]
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits([0, 1, 2, 3]),
        0,
    ));
    out.extend_from_slice(&values);
    out
}

fn imm32_scalar(value: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_IMMEDIATE32, 1, OPERAND_SEL_SELECT1, 0, 0),
        value,
    ]
}

fn assert_wgsl_parses(wgsl: &str) {
    naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
}

#[test]
fn decodes_and_translates_setp_and_predicated_mov() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
    //
    // Compare op encoding used by the decoder:
    // 5 = Gt.
    let dst_p0x = reg_dst(OPERAND_TYPE_PREDICATE, 0, WriteMask::X);
    let src_v0x = reg_src(OPERAND_TYPE_INPUT, 0, [0, 0, 0, 0]);
    let src_zero = imm32_scalar(0.0f32.to_bits());
    let setp_len = 1 + dst_p0x.len() as u32 + src_v0x.len() as u32 + src_zero.len() as u32;
    body.push(opcode_token_setp(setp_len, 5));
    body.extend_from_slice(&dst_p0x);
    body.extend_from_slice(&src_v0x);
    body.extend_from_slice(&src_zero);

    // mov o0, l(0, 0, 0, 0)
    let imm0 = imm32_vec4([0u32; 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm0.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm0);

    // (+p0.x) setp p1.x, v0.x, l(1.0), gt
    let dst_p1x = reg_dst(OPERAND_TYPE_PREDICATE, 1, WriteMask::X);
    let src_one = imm32_scalar(1.0f32.to_bits());
    let pred_p0x = pred_operand(0, 0);
    let pred_setp_len = 1
        + pred_p0x.len() as u32
        + dst_p1x.len() as u32
        + src_v0x.len() as u32
        + src_one.len() as u32;
    body.push(opcode_token_setp(pred_setp_len, 5));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&dst_p1x);
    body.extend_from_slice(&src_v0x);
    body.extend_from_slice(&src_one);

    // (+p1.x) mov o0, l(1, 1, 1, 1)
    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    let pred = pred_operand(1, 0);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + pred.len() as u32 + 2 + imm1.len() as u32,
    ));
    body.extend_from_slice(&pred);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm1);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (
            FOURCC_ISGN,
            build_signature_chunk(&[sig_param("TEXCOORD", 0, 0, 0b1111)]),
        ),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");

    assert!(matches!(module.instructions[0], Sm4Inst::Setp { .. }));
    assert!(matches!(module.instructions[2], Sm4Inst::Predicated { .. }));
    assert!(matches!(module.instructions[3], Sm4Inst::Predicated { .. }));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);

    assert!(
        translated.wgsl.contains("var p0: vec4<bool>"),
        "expected predicate register decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("var p1: vec4<bool>"),
        "expected predicate register decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (p0.x) {"),
        "expected predicated setp to translate to if(p0.x):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (p1.x) {"),
        "expected predicated mov to translate to if(p1.x):\n{}",
        translated.wgsl
    );
}
