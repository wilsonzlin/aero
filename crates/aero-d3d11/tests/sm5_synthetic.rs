use aero_d3d11::{FourCC, Sm4Program, translate_sm4_to_wgsl};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn make_dxbc_with_single_chunk(fourcc: FourCC, chunk_data: &[u8]) -> Vec<u8> {
    let header_size = 4 + 16 + 4 + 4 + 4 + 4; // magic + checksum + one + total + count + offset[0]
    let chunk_offset = header_size;
    let total_size = header_size + 8 + chunk_data.len();

    let mut bytes = Vec::with_capacity(total_size);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored by our parser)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // "one"
    bytes.extend_from_slice(&(total_size as u32).to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes()); // chunk count
    bytes.extend_from_slice(&(chunk_offset as u32).to_le_bytes());

    bytes.extend_from_slice(&fourcc.0);
    bytes.extend_from_slice(&(chunk_data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(chunk_data);

    bytes
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout assumed by our decoder:
    // type in bits 16.., major in bits 4..7, minor in bits 0..3.
    let version = ((stage_type as u32) << 16) | (5u32 << 4) | 0u32;
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
    opcode | (len << 11)
}

fn operand_token(operand_type: u32) -> u32 {
    // Our minimal operand decoder reads type from bits 4..=11.
    operand_type << 4
}

fn assert_wgsl_parses(wgsl: &str) {
    naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
}

#[test]
fn translates_synthetic_sm5_vertex_passthrough() {
    // Opcodes are currently hard-coded in the translator's bootstrap decoder.
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v0
    let mov0 = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        0,
        operand_token(OPERAND_INPUT),
        0,
    ];
    // mov o1, v1
    let mov1 = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        1,
        operand_token(OPERAND_INPUT),
        1,
    ];
    // ret
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 1 is assumed to be vertex by our current decoder.
    let tokens = make_sm5_program_tokens(1, &[mov0.as_slice(), mov1.as_slice(), ret.as_slice()].concat());
    let dxbc = make_dxbc_with_single_chunk(FOURCC_SHEX, &tokens_to_bytes(&tokens));

    let program = Sm4Program::parse_from_dxbc_bytes(&dxbc).expect("SM4/5 parse failed");
    assert_eq!(program.model.major, 5);

    let wgsl = translate_sm4_to_wgsl(&program).expect("translation failed").wgsl;
    assert_wgsl_parses(&wgsl);
    assert!(wgsl.contains("@vertex"));
    assert!(wgsl.contains("out.pos = input.v0"));
    assert!(wgsl.contains("out.o1 = input.v1"));
}

#[test]
fn translates_synthetic_sm5_pixel_passthrough() {
    const OPCODE_MOV: u32 = 0x01;
    const OPCODE_RET: u32 = 0x3e;

    const OPERAND_INPUT: u32 = 1;
    const OPERAND_OUTPUT: u32 = 2;

    // mov o0, v1  (return COLOR0)
    let mov = [
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_OUTPUT),
        0,
        operand_token(OPERAND_INPUT),
        1,
    ];
    let ret = [opcode_token(OPCODE_RET, 1)];

    // Stage type 0 is assumed to be pixel by our current decoder.
    let tokens = make_sm5_program_tokens(0, &[mov.as_slice(), ret.as_slice()].concat());
    let dxbc = make_dxbc_with_single_chunk(FOURCC_SHEX, &tokens_to_bytes(&tokens));

    let program = Sm4Program::parse_from_dxbc_bytes(&dxbc).expect("SM4/5 parse failed");
    assert_eq!(program.model.major, 5);

    let wgsl = translate_sm4_to_wgsl(&program).expect("translation failed").wgsl;
    assert_wgsl_parses(&wgsl);
    assert!(wgsl.contains("@fragment"));
    assert!(wgsl.contains("return input.v1"));
}
