use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcSignatureParameter, FourCC, Sm4CmpOp,
    Sm4Inst, Sm4Program, WriteMask,
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
            min_precision: u32::from(p.min_precision),
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

fn opcode_token_with_test(opcode: u32, len: u32, test: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT) | ((test & OPCODE_TEST_MASK) << OPCODE_TEST_SHIFT)
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

fn pred_operand_neg(idx: u32, component: u32) -> Vec<u32> {
    // Operand modifier encoding:
    // - Operand token has OPERAND_EXTENDED_BIT set, followed by one extended operand token.
    // - Extended operand token type is in bits 0..=5 (0 = modifier token).
    // - Modifier is stored in bits 6..=7 (1 = neg).
    vec![
        operand_token(OPERAND_TYPE_PREDICATE, 1, OPERAND_SEL_SELECT1, component, 1)
            | OPERAND_EXTENDED_BIT,
        1u32 << 6, // type=0, modifier=neg
        idx,
    ]
}

fn resource_operand(slot: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_RESOURCE, 0, OPERAND_SEL_MASK, 0, 1),
        slot,
    ]
}

fn sampler_operand(slot: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_SAMPLER, 0, OPERAND_SEL_MASK, 0, 1),
        slot,
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

fn pred_operand_mask(idx: u32, mask: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_PREDICATE, 1, OPERAND_SEL_MASK, mask, 1),
        idx,
    ]
}

fn pred_operand_swizzle_neg(idx: u32, swizzle: [u8; 4]) -> Vec<u32> {
    // Encode a predicate operand using SWIZZLE selection and the extended operand modifier token.
    //
    // This exercises the decoder paths for:
    // - `OPERAND_SEL_SWIZZLE` scalar predicates (replicated swizzle), and
    // - `OperandModifier::Neg` inversion (e.g. `(-p0.x)`).
    let mut tok = operand_token(
        OPERAND_TYPE_PREDICATE,
        1,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(swizzle),
        1,
    );
    tok |= OPERAND_EXTENDED_BIT;
    let ext = 1u32 << 6; // modifier = Neg
    vec![tok, ext, idx]
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
    let pred_p1x = pred_operand(1, 0);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + pred_p1x.len() as u32 + 2 + imm1.len() as u32,
    ));
    body.extend_from_slice(&pred_p1x);
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

    assert!(matches!(&module.instructions[0], Sm4Inst::Setp { .. }));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated {
            inner,
            ..
        } if matches!(inner.as_ref(), Sm4Inst::Setp { .. })
    ));
    assert!(matches!(
        &module.instructions[3],
        Sm4Inst::Predicated {
            inner,
            ..
        } if matches!(inner.as_ref(), Sm4Inst::Mov { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

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

#[test]
fn decodes_and_translates_trailing_predicated_mov() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
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

    // mov o0, l(1, 1, 1, 1) (+p0.x)
    //
    // Some SM4/SM5 blobs encode the predicate operand as the final operand rather than as a
    // leading `(+p0.x)` operand. Ensure the decoder accepts this form.
    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    let pred_p0x = pred_operand(0, 0);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + 2 + imm1.len() as u32 + pred_p0x.len() as u32,
    ));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm1);
    body.extend_from_slice(&pred_p0x);

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

    assert!(matches!(&module.instructions[0], Sm4Inst::Setp { .. }));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated {
            inner,
            ..
        } if matches!(inner.as_ref(), Sm4Inst::Mov { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("var p0: vec4<bool>"),
        "expected predicate register decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (p0.x) {"),
        "expected predicated mov to translate to if(p0.x):\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_setp_unordered_float_cmp_uses_nan_handling() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, l(NaN), l(0.0), lt_u
    //
    // In `D3D10_SB_INSTRUCTION_COMPARISON`, the `_U` suffix means "unordered" float compare:
    // comparisons are true if either operand is NaN.
    let dst_p0x = reg_dst(OPERAND_TYPE_PREDICATE, 0, WriteMask::X);
    let src_nan = imm32_scalar(0x7fc0_0000u32);
    let src_zero = imm32_scalar(0.0f32.to_bits());
    // Compare op encoding used by the decoder: 10 = LtU.
    let setp_len = 1 + dst_p0x.len() as u32 + src_nan.len() as u32 + src_zero.len() as u32;
    body.push(opcode_token_setp(setp_len, 10));
    body.extend_from_slice(&dst_p0x);
    body.extend_from_slice(&src_nan);
    body.extend_from_slice(&src_zero);

    // mov o0, l(0, 0, 0, 0)
    let imm0 = imm32_vec4([0u32; 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm0.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm0);

    // (+p0.x) mov o0, l(1, 1, 1, 1)
    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    let pred_p0x = pred_operand(0, 0);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + pred_p0x.len() as u32 + 2 + imm1.len() as u32,
    ));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm1);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("!= ((setp_a_0).x)")
            || translated.wgsl.contains("!= ((setp_b_0).x)"),
        "expected unordered setp compare to include NaN handling (`x != x`):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (p0.x) {"),
        "expected predicated mov to translate to if(p0.x):\n{}",
        translated.wgsl
    );
}
#[test]
fn decodes_and_translates_inverted_predicated_mov() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
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

    // (-p0.x) mov o0, l(1, 1, 1, 1)
    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    let pred_neg_p0x = pred_operand_neg(0, 0);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + pred_neg_p0x.len() as u32 + 2 + imm1.len() as u32,
    ));
    body.extend_from_slice(&pred_neg_p0x);
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

    assert!(matches!(&module.instructions[0], Sm4Inst::Setp { .. }));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated {
            pred,
            inner,
            ..
        } if pred.invert && matches!(inner.as_ref(), Sm4Inst::Mov { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("var p0: vec4<bool>"),
        "expected predicate register decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (!(p0.x)) {"),
        "expected inverted predicated mov to translate to if(!(p0.x)):\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_trailing_inverted_predicated_mov() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
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

    // mov o0, l(1, 1, 1, 1) (-p0.x)
    //
    // Ensure trailing predicate operands can also carry a negate modifier via an extended operand
    // token.
    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    let pred_neg_p0x = pred_operand_neg(0, 0);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + 2 + imm1.len() as u32 + pred_neg_p0x.len() as u32,
    ));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm1);
    body.extend_from_slice(&pred_neg_p0x);

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

    assert!(matches!(&module.instructions[0], Sm4Inst::Setp { .. }));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated {
            pred,
            inner,
            ..
        } if pred.invert && matches!(inner.as_ref(), Sm4Inst::Mov { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("var p0: vec4<bool>"),
        "expected predicate register decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (!(p0.x)) {"),
        "expected inverted predicated mov to translate to if(!(p0.x)):\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_trailing_predicated_setp() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
    let dst_p0x = reg_dst(OPERAND_TYPE_PREDICATE, 0, WriteMask::X);
    let src_v0x = reg_src(OPERAND_TYPE_INPUT, 0, [0, 0, 0, 0]);
    let src_zero = imm32_scalar(0.0f32.to_bits());
    let setp_len = 1 + dst_p0x.len() as u32 + src_v0x.len() as u32 + src_zero.len() as u32;
    body.push(opcode_token_setp(setp_len, 5));
    body.extend_from_slice(&dst_p0x);
    body.extend_from_slice(&src_v0x);
    body.extend_from_slice(&src_zero);

    // setp p1.x, v0.y, l(0.0), gt (+p0.x)
    //
    // Unlike the leading-operand encoding, `setp` with trailing predication starts with the
    // predicate destination and ends with the predicate condition.
    let dst_p1x = reg_dst(OPERAND_TYPE_PREDICATE, 1, WriteMask::X);
    let src_v0y = reg_src(OPERAND_TYPE_INPUT, 0, [1, 1, 1, 1]);
    let pred_p0x = pred_operand(0, 0);
    let setp2_len = 1
        + dst_p1x.len() as u32
        + src_v0y.len() as u32
        + src_zero.len() as u32
        + pred_p0x.len() as u32;
    body.push(opcode_token_setp(setp2_len, 5));
    body.extend_from_slice(&dst_p1x);
    body.extend_from_slice(&src_v0y);
    body.extend_from_slice(&src_zero);
    body.extend_from_slice(&pred_p0x);

    // mov o0, l(0, 0, 0, 0)
    let imm0 = imm32_vec4([0u32; 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm0.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm0);

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

    assert!(matches!(&module.instructions[0], Sm4Inst::Setp { .. }));
    assert!(matches!(
        &module.instructions[1],
        Sm4Inst::Predicated {
            inner,
            ..
        } if matches!(inner.as_ref(), Sm4Inst::Setp { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("var p1: vec4<bool>"),
        "expected p1 predicate register decl in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (p0.x) {"),
        "expected predicated setp to translate to if(p0.x):\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_ret() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
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

    // (+p0.x) ret
    let pred_p0x = pred_operand(0, 0);
    body.push(opcode_token(OPCODE_RET, 1 + pred_p0x.len() as u32));
    body.extend_from_slice(&pred_p0x);

    // mov o0, l(1, 1, 1, 1)
    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm1.len() as u32));
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

    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated { .. }
    ));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated {
            inner,
            ..
        } if matches!(inner.as_ref(), Sm4Inst::Ret)
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("if (p0.x) {"),
        "expected predicated ret to translate to if(p0.x):\n{}",
        translated.wgsl
    );
    let return_count = translated.wgsl.match_indices("return out;").count();
    assert!(
        return_count >= 2,
        "expected predicated ret to emit an additional return out (count={return_count}):\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_breakc_in_loop() {
    // loop
    //   (+p0.x) breakc_eq l(0.0), l(0.0)
    //   break
    // endloop
    // mov o0, l(1,1,1,1)
    // ret
    let mut body = Vec::<u32>::new();

    body.push(opcode_token(OPCODE_LOOP, 1));

    let pred_p0x = pred_operand(0, 0);
    let a = imm32_scalar(0.0f32.to_bits());
    let b = imm32_scalar(0.0f32.to_bits());
    body.push(opcode_token_with_test(
        OPCODE_BREAKC,
        1 + pred_p0x.len() as u32 + a.len() as u32 + b.len() as u32,
        2, // eq (D3D10_SB_INSTRUCTION_TEST)
    ));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&a);
    body.extend_from_slice(&b);

    // Ensure the loop can terminate even if the predicate is false.
    body.push(opcode_token(OPCODE_BREAK, 1));
    body.push(opcode_token(OPCODE_ENDLOOP, 1));

    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm1.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm1);

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

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");

    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::BreakC { .. }))));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("if (p0.x &&") && translated.wgsl.contains("break;"),
        "expected predicated breakc to lower via if (p0.x && cmp) {{ break; }}:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_continuec_in_loop() {
    // loop
    //   (+p0.x) continuec_eq l(0.0), l(1.0)  (false, but exercises translation)
    //   break
    // endloop
    // mov o0, l(1,1,1,1)
    // ret
    let mut body = Vec::<u32>::new();

    body.push(opcode_token(OPCODE_LOOP, 1));

    let pred_p0x = pred_operand(0, 0);
    let a = imm32_scalar(0.0f32.to_bits());
    let b = imm32_scalar(1.0f32.to_bits());
    body.push(opcode_token_with_test(
        OPCODE_CONTINUEC,
        1 + pred_p0x.len() as u32 + a.len() as u32 + b.len() as u32,
        2, // eq (D3D10_SB_INSTRUCTION_TEST)
    ));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&a);
    body.extend_from_slice(&b);

    // Exit the loop.
    body.push(opcode_token(OPCODE_BREAK, 1));
    body.push(opcode_token(OPCODE_ENDLOOP, 1));

    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm1.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm1);

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

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");

    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::ContinueC { .. }))));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("if (p0.x &&") && translated.wgsl.contains("continue;"),
        "expected predicated continuec to lower via if (p0.x && cmp) {{ continue; }}:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_breakc_in_switch_case() {
    // switch l(0)
    //   case 0:
    //     (+p0.x) breakc_eq l(0.0), l(0.0)
    // endswitch
    // ret
    //
    // This exercises the `inside_case` path for predicated `breakc` lowering.
    let mut body = Vec::<u32>::new();

    let selector = imm32_scalar(0);
    body.push(opcode_token(OPCODE_SWITCH, 1 + selector.len() as u32));
    body.extend_from_slice(&selector);

    let case0 = imm32_scalar(0);
    body.push(opcode_token(OPCODE_CASE, 1 + case0.len() as u32));
    body.extend_from_slice(&case0);

    let pred_p0x = pred_operand(0, 0);
    let a = imm32_scalar(0.0f32.to_bits());
    let b = imm32_scalar(0.0f32.to_bits());
    body.push(opcode_token_with_test(
        OPCODE_BREAKC,
        1 + pred_p0x.len() as u32 + a.len() as u32 + b.len() as u32,
        2, // eq (D3D10_SB_INSTRUCTION_TEST)
    ));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&a);
    body.extend_from_slice(&b);

    body.push(opcode_token(OPCODE_ENDSWITCH, 1));
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

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");

    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Switch { .. })));
    assert!(module
        .instructions
        .iter()
        .any(|i| matches!(i, Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::BreakC { .. }))));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let wgsl = &translated.wgsl;
    assert!(wgsl.contains("switch("), "expected switch in WGSL:\n{wgsl}");
    let idx_case0 = wgsl.find("case 0i").expect("case 0");
    let idx_default = wgsl.find("default:").expect("default");
    let case0_body = &wgsl[idx_case0..idx_default];
    assert!(
        case0_body.contains("p0.x") && case0_body.contains("break;"),
        "expected predicated conditional break inside switch case:\n{wgsl}"
    );
}

#[test]
fn decodes_and_translates_predicate_operand_swizzle_mask_and_negation() {
    let mut body = Vec::<u32>::new();

    // setp p0.xy, v0.x, l(0.0), gt
    let dst_p0xy = reg_dst(OPERAND_TYPE_PREDICATE, 0, WriteMask(0b0011));
    let src_v0x = reg_src(OPERAND_TYPE_INPUT, 0, [0, 0, 0, 0]);
    let src_zero = imm32_scalar(0.0f32.to_bits());
    let setp_len = 1 + dst_p0xy.len() as u32 + src_v0x.len() as u32 + src_zero.len() as u32;
    body.push(opcode_token_setp(setp_len, 5));
    body.extend_from_slice(&dst_p0xy);
    body.extend_from_slice(&src_v0x);
    body.extend_from_slice(&src_zero);

    // setp p1.x, l(1), l(2), lt_u
    //
    // Note: In SM4/SM5 token streams the `_U` suffix on comparisons denotes "unordered" float
    // compares (NaN-aware), not unsigned integer compares.
    let dst_p1x = reg_dst(OPERAND_TYPE_PREDICATE, 1, WriteMask::X);
    let imm_u32 = |v: u32| imm32_scalar(v);
    let a_u = imm_u32(1);
    let b_u = imm_u32(2);
    let setp_u_len = 1 + dst_p1x.len() as u32 + a_u.len() as u32 + b_u.len() as u32;
    body.push(opcode_token_setp(setp_u_len, 10));
    body.extend_from_slice(&dst_p1x);
    body.extend_from_slice(&a_u);
    body.extend_from_slice(&b_u);

    // (p0.y via MASK selection) mov o0, l(0.25, 0.25, 0.25, 0.25)
    let pred_p0y_mask = pred_operand_mask(0, 0b0010);
    let imm_quarter = imm32_vec4([0.25f32.to_bits(); 4]);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + pred_p0y_mask.len() as u32 + 2 + imm_quarter.len() as u32,
    ));
    body.extend_from_slice(&pred_p0y_mask);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm_quarter);

    // (-p0.xxxx via SWIZZLE selection + extended modifier) mov o0, l(0.5, 0.5, 0.5, 0.5)
    let pred_p0x_neg = pred_operand_swizzle_neg(0, [0, 0, 0, 0]);
    let imm_half = imm32_vec4([0.5f32.to_bits(); 4]);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + pred_p0x_neg.len() as u32 + 2 + imm_half.len() as u32,
    ));
    body.extend_from_slice(&pred_p0x_neg);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm_half);

    body.push(opcode_token(OPCODE_RET, 1));

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
    let Sm4Inst::Setp { dst, op, .. } = &module.instructions[0] else {
        panic!("expected first instruction to be setp");
    };
    assert_eq!(dst.reg.index, 0);
    assert_eq!(dst.mask.0, 0b0011);
    assert_eq!(*op, Sm4CmpOp::Gt);

    let Sm4Inst::Setp { dst, op, .. } = &module.instructions[1] else {
        panic!("expected second instruction to be setp");
    };
    assert_eq!(dst.reg.index, 1);
    assert_eq!(dst.mask.0, WriteMask::X.0);
    assert_eq!(*op, Sm4CmpOp::LtU);

    let Sm4Inst::Predicated { pred, .. } = &module.instructions[2] else {
        panic!("expected third instruction to be predicated mov");
    };
    assert_eq!(pred.reg.index, 0);
    assert_eq!(pred.component, 1); // y
    assert!(!pred.invert);

    let Sm4Inst::Predicated { pred, .. } = &module.instructions[3] else {
        panic!("expected fourth instruction to be predicated mov");
    };
    assert_eq!(pred.reg.index, 0);
    assert_eq!(pred.component, 0); // x
    assert!(pred.invert);

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

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
        translated.wgsl.contains("p0.x =") && translated.wgsl.contains("p0.y ="),
        "expected setp write mask to update p0.x and p0.y:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (p0.y) {"),
        "expected mask-selected predicate to gate on p0.y:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (!(p0.x)) {"),
        "expected negated predicate to emit if (!(p0.x)):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("!= ((setp_a_1).x)")
            || translated.wgsl.contains("!= ((setp_b_1).x)"),
        "expected lt_u setp to include unordered NaN handling (`x != x`):\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_setp_ordered_ne_is_false_when_nan_is_involved() {
    // `Sm4CmpOp::Ne` (no `_U` suffix) is the **ordered** not-equal variant in the SM4 tokenized
    // program format: it must evaluate to false when either operand is NaN.
    //
    // We can't execute the shader here, but we can assert that translation emits explicit NaN
    // guards (`x == x`) rather than using WGSL `!=` directly (which would be unordered).
    let mut body = Vec::<u32>::new();

    // setp p0.x, l(NaN), l(0.0), ne
    let dst_p0x = reg_dst(OPERAND_TYPE_PREDICATE, 0, WriteMask::X);
    let src_nan = imm32_scalar(0x7fc0_0000); // quiet NaN (f32)
    let src_zero = imm32_scalar(0.0f32.to_bits());
    let setp_len = 1 + dst_p0x.len() as u32 + src_nan.len() as u32 + src_zero.len() as u32;
    body.push(opcode_token_setp(setp_len, 1)); // Ne
    body.extend_from_slice(&dst_p0x);
    body.extend_from_slice(&src_nan);
    body.extend_from_slice(&src_zero);

    // (+p0.x) mov o0, l(1,1,1,1)
    let pred_p0x = pred_operand(0, 0);
    let imm1 = imm32_vec4([1.0f32.to_bits(); 4]);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + pred_p0x.len() as u32 + 2 + imm1.len() as u32,
    ));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm1);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // For ordered `ne`, the translator should emit `x == x` NaN guards.
    assert!(
        translated.wgsl.contains("((setp_a_0).x) == ((setp_a_0).x)"),
        "expected ordered ne setp to include NaN guard for lhs (`x == x`):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("((setp_b_0).x) == ((setp_b_0).x)"),
        "expected ordered ne setp to include NaN guard for rhs (`x == x`):\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_sample() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
    let dst_p0x = reg_dst(OPERAND_TYPE_PREDICATE, 0, WriteMask::X);
    let src_v0 = reg_src(OPERAND_TYPE_INPUT, 0, [0, 1, 2, 3]);
    let src_zero = imm32_scalar(0.0f32.to_bits());
    let setp_len = 1 + dst_p0x.len() as u32 + src_v0.len() as u32 + src_zero.len() as u32;
    body.push(opcode_token_setp(setp_len, 5));
    body.extend_from_slice(&dst_p0x);
    body.extend_from_slice(&src_v0);
    body.extend_from_slice(&src_zero);

    // (+p0.x) sample o0, v0.xy, t0, s0
    //
    // In the fragment stage, `sample` lowers to `textureSample` which has derivative uniformity
    // requirements in WGSL/WebGPU. Ensure our predication lowering evaluates the sample in uniform
    // control flow and only guards the destination write.
    let pred_p0x = pred_operand(0, 0);
    let dst_o0 = reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW);
    let tex_t0 = resource_operand(0);
    let samp_s0 = sampler_operand(0);
    let sample_len = 1
        + pred_p0x.len() as u32
        + dst_o0.len() as u32
        + src_v0.len() as u32
        + tex_t0.len() as u32
        + samp_s0.len() as u32;
    body.push(opcode_token(OPCODE_SAMPLE, sample_len));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&dst_o0);
    body.extend_from_slice(&src_v0);
    body.extend_from_slice(&tex_t0);
    body.extend_from_slice(&samp_s0);

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

    assert!(matches!(
        &module.instructions[1],
        Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::Sample { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let sample_pos = translated
        .wgsl
        .find("textureSample(")
        .expect("expected textureSample call in WGSL");
    let if_pos = translated
        .wgsl
        .find("if (p0.x) {")
        .expect("expected predication if guard in WGSL");
    assert!(
        sample_pos < if_pos,
        "expected textureSample to be emitted before the predication guard:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_trailing_predicated_sample() {
    let mut body = Vec::<u32>::new();

    // setp p0.x, v0.x, l(0.0), gt
    let dst_p0x = reg_dst(OPERAND_TYPE_PREDICATE, 0, WriteMask::X);
    let src_v0 = reg_src(OPERAND_TYPE_INPUT, 0, [0, 1, 2, 3]);
    let src_zero = imm32_scalar(0.0f32.to_bits());
    let setp_len = 1 + dst_p0x.len() as u32 + src_v0.len() as u32 + src_zero.len() as u32;
    body.push(opcode_token_setp(setp_len, 5));
    body.extend_from_slice(&dst_p0x);
    body.extend_from_slice(&src_v0);
    body.extend_from_slice(&src_zero);

    // sample o0, v0.xy, t0, s0 (+p0.x)
    //
    // Some blobs encode the predicate operand at the end of the operand list rather than in the
    // usual `(+p0.x)` prefix form. Ensure the decoder recognizes this encoding for `sample` and the
    // translation still emits `textureSample` outside the predication `if`.
    let dst_o0 = reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW);
    let tex_t0 = resource_operand(0);
    let samp_s0 = sampler_operand(0);
    let pred_p0x = pred_operand(0, 0);
    let sample_len = 1
        + dst_o0.len() as u32
        + src_v0.len() as u32
        + tex_t0.len() as u32
        + samp_s0.len() as u32
        + pred_p0x.len() as u32;
    body.push(opcode_token(OPCODE_SAMPLE, sample_len));
    body.extend_from_slice(&dst_o0);
    body.extend_from_slice(&src_v0);
    body.extend_from_slice(&tex_t0);
    body.extend_from_slice(&samp_s0);
    body.extend_from_slice(&pred_p0x);

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

    assert!(matches!(
        &module.instructions[1],
        Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::Sample { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let sample_pos = translated
        .wgsl
        .find("textureSample(")
        .expect("expected textureSample call in WGSL");
    let if_pos = translated
        .wgsl
        .find("if (p0.x) {")
        .expect("expected predication if guard in WGSL");
    assert!(
        sample_pos < if_pos,
        "expected textureSample to be emitted before the predication guard:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_break_in_loop() {
    let mut body = Vec::<u32>::new();

    // mov o0, l(0, 0, 0, 0)
    let imm0 = imm32_vec4([0u32; 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm0.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm0);

    // loop
    body.push(opcode_token(OPCODE_LOOP, 1));

    // (+p0.x) break
    let pred_p0x = pred_operand(0, 0);
    body.push(opcode_token(OPCODE_BREAK, 1 + pred_p0x.len() as u32));
    body.extend_from_slice(&pred_p0x);

    // break (ensure the loop terminates even when p0.x is false)
    body.push(opcode_token(OPCODE_BREAK, 1));

    // endloop
    body.push(opcode_token(OPCODE_ENDLOOP, 1));

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

    assert!(matches!(&module.instructions[1], Sm4Inst::Loop));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::Break)
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("loop {"),
        "expected loop block in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("if (p0.x) {") && translated.wgsl.contains("break;"),
        "expected predicated break to lower via if (p0.x) {{ break; }}:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_continue_in_loop() {
    let mut body = Vec::<u32>::new();

    // mov o0, l(0, 0, 0, 0)
    let imm0 = imm32_vec4([0u32; 4]);
    body.push(opcode_token(OPCODE_MOV, 1 + 2 + imm0.len() as u32));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm0);

    // loop
    body.push(opcode_token(OPCODE_LOOP, 1));

    // (+p0.x) continue
    let pred_p0x = pred_operand(0, 0);
    body.push(opcode_token(OPCODE_CONTINUE, 1 + pred_p0x.len() as u32));
    body.extend_from_slice(&pred_p0x);

    // break (avoid an infinite loop when p0.x is false)
    body.push(opcode_token(OPCODE_BREAK, 1));

    // endloop
    body.push(opcode_token(OPCODE_ENDLOOP, 1));

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

    assert!(matches!(&module.instructions[1], Sm4Inst::Loop));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::Continue)
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("if (p0.x) {") && translated.wgsl.contains("continue;"),
        "expected predicated continue to lower via if (p0.x) {{ continue; }}:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_break_in_switch_case() {
    let mut body = Vec::<u32>::new();

    // switch l(0)
    let selector = imm32_scalar(0);
    body.push(opcode_token(OPCODE_SWITCH, 1 + selector.len() as u32));
    body.extend_from_slice(&selector);

    // case 0
    let case0 = imm32_scalar(0);
    body.push(opcode_token(OPCODE_CASE, 1 + case0.len() as u32));
    body.extend_from_slice(&case0);

    // (+p0.x) break
    let pred_p0x = pred_operand(0, 0);
    body.push(opcode_token(OPCODE_BREAK, 1 + pred_p0x.len() as u32));
    body.extend_from_slice(&pred_p0x);

    // default
    body.push(opcode_token(OPCODE_DEFAULT, 1));

    // endswitch
    body.push(opcode_token(OPCODE_ENDSWITCH, 1));

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

    assert!(matches!(&module.instructions[0], Sm4Inst::Switch { .. }));
    assert!(matches!(&module.instructions[1], Sm4Inst::Case { .. }));
    assert!(matches!(
        &module.instructions[2],
        Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::Break)
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("switch(")
            && translated.wgsl.contains("case 0i")
            && translated.wgsl.contains("if (p0.x) {")
            && translated.wgsl.contains("break;"),
        "expected predicated break in switch case to translate to if(p0.x) break:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_resinfo() {
    let mut body = Vec::<u32>::new();

    // dcl_resource_texture2d t0
    //
    // `resinfo` translation requires a matching `dcl_resource` declaration so the backend can
    // distinguish `Texture2D` from other resource dimensions.
    let tex_t0 = resource_operand(0);
    let dcl_len = 1 + tex_t0.len() as u32 + 1;
    body.push(opcode_token(OPCODE_DCL_RESOURCE, dcl_len));
    body.extend_from_slice(&tex_t0);
    body.push(2); // dimensionality = Texture2D

    // (+p0.x) resinfo r0.xyzw, l(0), t0
    //
    // This specifically exercises predication lowering for instructions that depend on module
    // declarations. The predication wrapper must preserve `module.decls` when emitting the inner
    // instruction.
    let pred_p0x = pred_operand(0, 0);
    let dst_r0 = reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW);
    let mip = imm32_scalar(0);
    let resinfo_len =
        1 + pred_p0x.len() as u32 + dst_r0.len() as u32 + mip.len() as u32 + tex_t0.len() as u32;
    body.push(opcode_token(OPCODE_RESINFO, resinfo_len));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&dst_r0);
    body.extend_from_slice(&mip);
    body.extend_from_slice(&tex_t0);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");

    assert!(matches!(
        &module.instructions[0],
        Sm4Inst::Predicated { inner, .. } if matches!(inner.as_ref(), Sm4Inst::ResInfo { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("if (p0.x) {"),
        "expected predicated resinfo to emit an if guard:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("textureDimensions(t0")
            && translated.wgsl.contains("textureNumLevels(t0"),
        "expected resinfo to query dimensions/levels:\n{}",
        translated.wgsl
    );
}

#[test]
fn decodes_and_translates_predicated_ld_structured() {
    let mut body = Vec::<u32>::new();

    // dcl_resource_structured t0, 16
    //
    // Structured buffer loads require the element stride from the corresponding declaration.
    let buf_t0 = resource_operand(0);
    let dcl_len = 1 + buf_t0.len() as u32 + 1;
    body.push(opcode_token(OPCODE_DCL_RESOURCE_STRUCTURED, dcl_len));
    body.extend_from_slice(&buf_t0);
    body.push(16); // stride bytes

    // (+p0.x) ld_structured r0.xyzw, l(0), l(0), t0
    //
    // This exercises predication lowering for structured buffer ops, which depend on
    // `dcl_resource_structured` metadata.
    let pred_p0x = pred_operand(0, 0);
    let dst_r0 = reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW);
    let index = imm32_scalar(0);
    let offset = imm32_scalar(0);
    let ld_len = 1
        + pred_p0x.len() as u32
        + dst_r0.len() as u32
        + index.len() as u32
        + offset.len() as u32
        + buf_t0.len() as u32;
    body.push(opcode_token(OPCODE_LD_STRUCTURED, ld_len));
    body.extend_from_slice(&pred_p0x);
    body.extend_from_slice(&dst_r0);
    body.extend_from_slice(&index);
    body.extend_from_slice(&offset);
    body.extend_from_slice(&buf_t0);

    // mov o0, r0
    let dst_o0 = reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW);
    let src_r0 = reg_src(OPERAND_TYPE_TEMP, 0, [0, 1, 2, 3]);
    body.push(opcode_token(
        OPCODE_MOV,
        1 + dst_o0.len() as u32 + src_r0.len() as u32,
    ));
    body.extend_from_slice(&dst_o0);
    body.extend_from_slice(&src_r0);

    body.push(opcode_token(OPCODE_RET, 1));

    // Stage type 0 is pixel shader.
    let tokens = make_sm5_program_tokens(0, &body);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, tokens_to_bytes(&tokens)),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    let dxbc = aero_d3d11::DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    let module = aero_d3d11::sm4::decode_program(&program).expect("SM4 decode");

    assert!(matches!(
        &module.instructions[0],
        Sm4Inst::Predicated { inner, .. }
            if matches!(inner.as_ref(), Sm4Inst::LdStructured { .. })
    ));

    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("if (p0.x) {"),
        "expected predicated ld_structured to emit an if guard:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("t0.data["),
        "expected structured load to index t0 storage buffer:\n{}",
        translated.wgsl
    );
}
