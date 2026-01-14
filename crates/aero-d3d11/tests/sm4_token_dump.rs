use aero_d3d11::sm4::opcode::OPCODE_RET;
use aero_d3d11::sm4::token_dump::tokenize_instructions;
use aero_d3d11::Sm4Program;

const PS_ADD_DXBC: &[u8] = include_bytes!("fixtures/ps_add.dxbc");

#[test]
fn token_dump_scans_fixture_without_panicking() {
    let program = Sm4Program::parse_from_dxbc_bytes(PS_ADD_DXBC).expect("fixture should parse");
    let insts =
        tokenize_instructions(&program.tokens).expect("tokenize_instructions should succeed");

    assert!(!insts.is_empty(), "expected at least one instruction");
    assert!(
        insts.iter().any(|i| i.ext_tokens.len() > 0),
        "expected ps_add fixture to contain an extended opcode token (e.g. add_sat)"
    );
    assert_eq!(
        insts.last().unwrap().opcode,
        OPCODE_RET,
        "expected final instruction to be ret"
    );
}
