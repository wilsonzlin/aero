use aero_d3d11::sm4::opcode::*;
use aero_d3d11::sm4::decode_program;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, DxbcFile, FourCC, ShaderSignatures, Sm4Inst, Sm4Program,
    WriteMask,
};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
// `bufinfo` opcode IDs are not currently modeled explicitly; the decoder recognizes the operand
// pattern structurally. Use an unused opcode ID (< 0x100 so it is treated as an instruction) to
// exercise the fallback path.
const OPCODE_TEST_BUFINFO: u32 = 0x3b;

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test");
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
    let mut body = Vec::<u32>::new();

    // dcl_thread_group 1, 1, 1
    body.push(opcode_token(OPCODE_DCL_THREAD_GROUP, 4));
    body.push(1);
    body.push(1);
    body.push(1);

    // bufinfo r0.x, t0
    body.push(opcode_token(OPCODE_TEST_BUFINFO, 1 + 2 + 2));
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
    assert!(matches!(module.instructions[0], Sm4Inst::BufInfoRaw { .. }));

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
    assert_wgsl_validates(&translated.wgsl);

}
