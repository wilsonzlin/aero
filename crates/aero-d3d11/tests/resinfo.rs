use aero_d3d11::binding_model::BINDING_BASE_TEXTURE;
use aero_d3d11::sm4::decode_program;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BindingKind, DxbcFile, FourCC, ShaderSignatures, Sm4Decl,
    Sm4Inst, Sm4Program, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

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

fn imm32_scalar(value: u32) -> Vec<u32> {
    vec![
        operand_token(
            OPERAND_TYPE_IMMEDIATE32,
            1,
            OPERAND_SEL_SELECT1,
            0,
            OPERAND_INDEX_DIMENSION_0D,
        ),
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

#[test]
fn decodes_and_translates_resinfo_for_texture2d() {
    // dcl_thread_group 1, 1, 1
    let mut body = vec![opcode_token(OPCODE_DCL_THREAD_GROUP, 4), 1, 1, 1];

    // dcl_resource_texture2d t0
    //
    // The decoder looks at the extra dimension token and maps `dim=2` to `ResourceTexture2D`.
    let tex_decl = reg_src_resource(0);
    body.push(opcode_token(
        OPCODE_DCL_RESOURCE,
        1 + tex_decl.len() as u32 + 1, // + dimension token
    ));
    body.extend_from_slice(&tex_decl);
    body.push(2);

    // resinfo r0.xyzw, l(3), t0
    body.push(opcode_token(OPCODE_RESINFO, 1 + 2 + 2 + 2));
    body.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    body.extend_from_slice(&imm32_scalar(3));
    body.extend_from_slice(&reg_src_resource(0));

    body.push(opcode_token(OPCODE_RET, 1));

    let tokens = make_sm5_program_tokens(5, &body);
    let dxbc_bytes =
        dxbc_test_utils::build_container_owned(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");

    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");
    assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);

    let module = decode_program(&program).expect("SM4 decode");
    assert!(
        module
            .decls
            .iter()
            .any(|d| matches!(d, Sm4Decl::ResourceTexture2D { slot: 0 })),
        "expected decoder to emit ResourceTexture2D decl for t0"
    );
    assert!(matches!(&module.instructions[0], Sm4Inst::ResInfo { .. }));

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
        .expect("translate");

    assert!(
        translated.wgsl.contains("textureDimensions(t0"),
        "expected resinfo to query texture dimensions:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("textureNumLevels(t0"),
        "expected resinfo to query mip level count:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected resinfo to store integer bits via bitcast:\n{}",
        translated.wgsl
    );
    let t0 = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Texture2D { slot: 0 }))
        .expect("expected reflection to include t0 Texture2D binding");
    assert_eq!(
        t0.group, 2,
        "expected compute-stage texture binding to use @group(2)"
    );
    assert_eq!(
        t0.binding, BINDING_BASE_TEXTURE,
        "expected t0 Texture2D binding to use BINDING_BASE_TEXTURE"
    );
    assert!(
        t0.visibility.contains(wgpu::ShaderStages::COMPUTE),
        "expected t0 Texture2D binding to have compute visibility"
    );
    assert_wgsl_validates(&translated.wgsl);
}
