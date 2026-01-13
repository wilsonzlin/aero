use aero_d3d11::sm4::{decode_program, opcode::*};
use aero_d3d11::{BufferKind, ShaderModel, ShaderStage, Sm4Decl, Sm4Program};

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn operand_token_1d_xyzw(ty: u32) -> u32 {
    // Minimal operand token for object-like operands (`t#`, `u#`): 0 components + mask selection + 1D
    // immediate index.
    (ty << OPERAND_TYPE_SHIFT)
        | (OPERAND_SEL_MASK << OPERAND_SELECTION_MODE_SHIFT)
        | (0xFu32 << OPERAND_COMPONENT_SELECTION_SHIFT)
        | (OPERAND_INDEX_DIMENSION_1D << OPERAND_INDEX_DIMENSION_SHIFT)
        | (OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX0_REP_SHIFT)
        | (OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX1_REP_SHIFT)
        | (OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX2_REP_SHIFT)
}

#[test]
fn decodes_structured_buffer_stride_from_decls() {
    // Hand-authored minimal cs_5_0 token stream:
    //   dcl_resource_structured t0, 16
    //   dcl_uav_structured u0, 32
    //   ret
    let version_token = (5u32 << 16) | (5u32 << 4); // cs_5_0 (type=5, major=5, minor=0)

    let mut tokens = vec![
        version_token,
        0, // patched below
        // dcl_resource_structured t0, 16
        opcode_token(OPCODE_DCL_RESOURCE_STRUCTURED, 4),
        operand_token_1d_xyzw(OPERAND_TYPE_RESOURCE),
        0,  // t0
        16, // stride_bytes
        // dcl_uav_structured u0, 32
        opcode_token(OPCODE_DCL_UAV_STRUCTURED, 4),
        operand_token_1d_xyzw(OPERAND_TYPE_UNORDERED_ACCESS_VIEW),
        0,  // u0
        32, // stride_bytes
        // ret
        opcode_token(OPCODE_RET, 1),
    ];
    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");

    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::ResourceBuffer { slot: 0, stride: 16, kind: BufferKind::Structured }
        )),
        "expected decoded ResourceStructured t0 stride 16"
    );
    assert!(
        module.decls.iter().any(|d| matches!(
            d,
            Sm4Decl::UavBuffer { slot: 0, stride: 32, kind: BufferKind::Structured }
        )),
        "expected decoded UavStructured u0 stride 32"
    );
}
