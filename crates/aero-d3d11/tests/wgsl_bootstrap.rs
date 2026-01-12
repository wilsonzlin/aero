use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{translate_sm4_to_wgsl_bootstrap, Sm4Program, WgslBootstrapError};

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

fn operand_token(operand_type: u32) -> u32 {
    operand_type << OPERAND_TYPE_SHIFT
}

#[test]
fn bootstrap_translates_mov_from_v0_as_position() {
    // Pixel shader stage type is 0.
    let body = [
        // mov o0, v0
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_TYPE_OUTPUT),
        0,
        operand_token(OPERAND_TYPE_INPUT),
        0,
        opcode_token(OPCODE_RET, 1),
    ];
    let program_bytes = tokens_to_bytes(&make_sm5_program_tokens(0, &body));
    let program = Sm4Program::parse_program_tokens(&program_bytes).expect("parse_program_tokens");

    let wgsl = translate_sm4_to_wgsl_bootstrap(&program)
        .expect("translation should succeed")
        .wgsl;
    assert!(wgsl.contains("return input.pos;"), "{wgsl}");
    assert!(!wgsl.contains("input.v0"), "{wgsl}");
    naga::front::wgsl::parse_str(&wgsl).expect("generated WGSL should parse");
}

#[test]
fn bootstrap_errors_on_unsupported_instruction() {
    // Pixel shader stage type is 0.
    let body = [
        // mov o0, v1
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_TYPE_OUTPUT),
        0,
        operand_token(OPERAND_TYPE_INPUT),
        1,
        // add (unsupported by bootstrap translator)
        opcode_token(OPCODE_ADD, 1),
        opcode_token(OPCODE_RET, 1),
    ];
    let program_bytes = tokens_to_bytes(&make_sm5_program_tokens(0, &body));
    let program = Sm4Program::parse_program_tokens(&program_bytes).expect("parse_program_tokens");

    let err = translate_sm4_to_wgsl_bootstrap(&program).expect_err("expected error");
    assert!(matches!(
        err,
        WgslBootstrapError::UnsupportedInstruction { opcode } if opcode == OPCODE_ADD
    ));
}

#[test]
fn bootstrap_ignores_nop_and_customdata_comment() {
    let body = [
        opcode_token(OPCODE_NOP, 1),
        // customdata comment block: opcode + class token.
        opcode_token(OPCODE_CUSTOMDATA, 2),
        0,
        // mov o0, v1
        opcode_token(OPCODE_MOV, 5),
        operand_token(OPERAND_TYPE_OUTPUT),
        0,
        operand_token(OPERAND_TYPE_INPUT),
        1,
        opcode_token(OPCODE_RET, 1),
    ];
    let program_bytes = tokens_to_bytes(&make_sm5_program_tokens(0, &body));
    let program = Sm4Program::parse_program_tokens(&program_bytes).expect("parse_program_tokens");

    let wgsl = translate_sm4_to_wgsl_bootstrap(&program)
        .expect("translation should succeed")
        .wgsl;
    assert!(wgsl.contains("@fragment"));
}
