use aero_d3d11::sm4::{decode_program, opcode::*};
use aero_d3d11::{ShaderModel, ShaderStage, Sm4Decl, Sm4Inst, Sm4Program};

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

#[test]
fn decodes_hs_dcl_inputcontrolpoints() {
    // hs_5_0:
    // - dcl_inputcontrolpoints 4
    // - ret
    let body = [
        opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2),
        4,
        opcode_token(OPCODE_RET, 1),
    ];

    let tokens = make_sm5_program_tokens(3, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.stage, ShaderStage::Hull);
    assert_eq!(
        module.model,
        ShaderModel {
            major: 5,
            minor: 0
        }
    );
    assert_eq!(
        module.decls,
        vec![Sm4Decl::InputControlPointCount { count: 4 }]
    );
    assert_eq!(module.instructions, vec![Sm4Inst::Ret]);
}

#[test]
fn decodes_ds_dcl_inputcontrolpoints() {
    // ds_5_0:
    // - dcl_inputcontrolpoints 3
    // - ret
    let body = [
        opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2),
        3,
        opcode_token(OPCODE_RET, 1),
    ];

    let tokens = make_sm5_program_tokens(4, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");
    let module = decode_program(&program).expect("decode");

    assert_eq!(module.stage, ShaderStage::Domain);
    assert_eq!(
        module.decls,
        vec![Sm4Decl::InputControlPointCount { count: 3 }]
    );
    assert_eq!(module.instructions, vec![Sm4Inst::Ret]);
}

#[test]
fn rejects_truncated_dcl_inputcontrolpoints() {
    // `dcl_inputcontrolpoints` has a fixed length of 2 DWORDs (opcode + count). Ensure the decoder
    // rejects token streams that end early instead of panicking.
    let body = [opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2)];

    let tokens = make_sm5_program_tokens(3, &body);
    let program =
        Sm4Program::parse_program_tokens(&tokens_to_bytes(&tokens)).expect("parse_program_tokens");

    let err = decode_program(&program).expect_err("decode should fail");
    assert_eq!(err.at_dword, 2);
    assert!(matches!(
        err.kind,
        aero_d3d11::sm4::decode::Sm4DecodeErrorKind::InstructionOutOfBounds {
            start: 2,
            len: 2,
            ..
        }
    ));
}

