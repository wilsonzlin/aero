use aero_d3d9_shader::{D3d9Shader, Instruction, Opcode, ShaderParseError, ShaderStage};

fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

#[test]
fn malformed_empty_blob_errors() {
    let err = D3d9Shader::parse(&[]).unwrap_err();
    assert_eq!(err, ShaderParseError::Empty);
}

#[test]
fn malformed_invalid_byte_length_errors() {
    let bytes = vec![0u8; 5];
    let err = D3d9Shader::parse(&bytes).unwrap_err();
    assert_eq!(err, ShaderParseError::InvalidByteLength { len: 5 });
}

#[test]
fn malformed_invalid_version_token_errors() {
    let words = [0x0001_0200, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidVersionToken { token: 0x0001_0200 }
    );
}

#[test]
fn malformed_dxbc_missing_shader_chunk_errors() {
    // Valid DXBC container, but without a shader bytecode chunk (`SHEX`/`SHDR`).
    let chunk_offset = 36u32;
    let total_size = chunk_offset as usize + 8; // chunk header only
    let mut dxbc = Vec::with_capacity(total_size);
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&(total_size as u32).to_le_bytes());
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk count
    dxbc.extend_from_slice(&chunk_offset.to_le_bytes());
    dxbc.extend_from_slice(b"ISGN");
    dxbc.extend_from_slice(&0u32.to_le_bytes()); // chunk size

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert_eq!(err, ShaderParseError::DxbcMissingShaderChunk);
}

#[test]
fn rejects_oversized_bytecode() {
    // Ensure the debug/disassembler parser does not allocate unbounded memory on hostile input.
    let bytes = vec![0u8; 256 * 1024 + 4];
    let err = D3d9Shader::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, aero_d3d9_shader::ShaderParseError::BytecodeTooLarge { .. }),
        "{err:?}"
    );
}

const VS_2_0_PASSTHROUGH: [u32; 14] = [
    0xFFFE_0200, // vs_2_0
    // dcl_position v0
    0x0200_001F,
    0x8000_0000,
    0x900F_0000,
    // dcl_texcoord0 v1
    0x0200_001F,
    0x8000_0005,
    0x900F_0001,
    // mov oPos, v0
    0x0200_0001,
    0xC00F_0000,
    0x90E4_0000,
    // mov oT0, v1
    0x0200_0001,
    0xE00F_0000,
    0x90E4_0001,
    // end
    0x0000_FFFF,
];

const PS_2_0_TEX_SAMPLE: [u32; 15] = [
    0xFFFF_0200, // ps_2_0
    // dcl_texcoord0 v0
    0x0200_001F,
    0x8000_0005,
    0x900F_0000,
    // dcl_2d s0
    0x0200_001F,
    0x9000_0000,
    0xA00F_0800,
    // texld r0, v0, s0
    0x0300_0042,
    0x800F_0000,
    0x90E4_0000,
    0xA0E4_0800,
    // mov oC0, r0
    0x0200_0001,
    0x800F_0800,
    0x80E4_0000,
    // end
    0x0000_FFFF,
];

const VS_3_0_IF: [u32; 30] = [
    0xFFFE_0300, // vs_3_0
    // dcl_position v0
    0x0200_001F,
    0x8000_0000,
    0x900F_0000,
    // dcl_position o0
    0x0200_001F,
    0x8000_0000,
    0xE00F_0000,
    // dcl_texcoord0 v1
    0x0200_001F,
    0x8000_0005,
    0x900F_0001,
    // dcl_texcoord0 o1
    0x0200_001F,
    0x8000_0005,
    0xE00F_0001,
    // mov o0, v0
    0x0200_0001,
    0xE00F_0000,
    0x90E4_0000,
    // mov o1, v1
    0x0200_0001,
    0xE00F_0001,
    0x90E4_0001,
    // setp p0.x, v0.x, c0.x
    0x0300_005E,
    0xB001_1000,
    0x9000_0000,
    0xA000_0000,
    // if p0.x
    0x0100_0028,
    0xB000_1000,
    // mov o1, v1
    0x0200_0001,
    0xE00F_0001,
    0x90E4_0001,
    // endif
    0x0000_002B,
    // end
    0x0000_FFFF,
];

const PS_3_0_TEXKILL: [u32; 17] = [
    0xFFFF_0300, // ps_3_0
    // dcl_texcoord0 v0
    0x0200_001F,
    0x8000_0005,
    0x900F_0000,
    // dcl_2d s0
    0x0200_001F,
    0x9000_0000,
    0xA00F_0800,
    // texld r0, v0, s0
    0x0300_0042,
    0x800F_0000,
    0x90E4_0000,
    0xA0E4_0800,
    // texkill r0
    0x0100_0041,
    0x80E4_0000,
    // mov oC0, r0
    0x0200_0001,
    0x800F_0800,
    0x80E4_0000,
    // end
    0x0000_FFFF,
];

#[test]
fn parse_vs_2_0_passthrough() {
    let shader = D3d9Shader::parse(&words_to_bytes(&VS_2_0_PASSTHROUGH)).unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.model.minor, 0);
    assert_eq!(shader.declarations.len(), 2);
    assert_eq!(shader.instructions.len(), 2);
    assert!(matches!(
        shader.instructions[0],
        Instruction::Op {
            opcode: Opcode::Mov,
            ..
        }
    ));
    let dis = shader.disassemble();
    assert!(dis.contains("vs_2_0"));
    assert!(dis.contains("dcl_position v0"));
    assert!(dis.contains("mov oPos, v0"));
}

#[test]
fn parse_ps_2_0_texture_sample() {
    let shader = D3d9Shader::parse(&words_to_bytes(&PS_2_0_TEX_SAMPLE)).unwrap();
    assert_eq!(shader.stage, ShaderStage::Pixel);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.declarations.len(), 2);
    assert_eq!(shader.instructions.len(), 2);
    assert!(matches!(
        shader.instructions[0],
        Instruction::Op {
            opcode: Opcode::Texld,
            ..
        }
    ));
    let dis = shader.disassemble();
    assert!(dis.contains("ps_2_0"));
    assert!(dis.contains("dcl_2d s0"));
    assert!(dis.contains("texld r0, v0, s0"));
    assert!(dis.contains("mov oC0, r0"));
}

#[test]
fn parse_vs_3_0_if() {
    let shader = D3d9Shader::parse(&words_to_bytes(&VS_3_0_IF)).unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 3);
    assert_eq!(shader.instructions.len(), 6);
    assert!(matches!(
        shader.instructions[2],
        Instruction::Op {
            opcode: Opcode::Setp,
            ..
        }
    ));
    assert!(matches!(
        shader.instructions[3],
        Instruction::Op {
            opcode: Opcode::If,
            ..
        }
    ));
    let dis = shader.disassemble();
    assert!(dis.contains("vs_3_0"));
    assert!(dis.contains("if p0"));
    assert!(dis.contains("endif"));
}

#[test]
fn parse_ps_3_0_texkill() {
    let shader = D3d9Shader::parse(&words_to_bytes(&PS_3_0_TEXKILL)).unwrap();
    assert_eq!(shader.stage, ShaderStage::Pixel);
    assert_eq!(shader.model.major, 3);
    assert_eq!(shader.instructions.len(), 3);
    assert!(matches!(
        shader.instructions[1],
        Instruction::Op {
            opcode: Opcode::Texkill,
            ..
        }
    ));
    let dis = shader.disassemble();
    assert!(dis.contains("texkill r0"));
}

#[test]
fn parse_dxbc_wrapped_sm2() {
    let shader_bytes = words_to_bytes(&VS_2_0_PASSTHROUGH);
    let chunk_offset = 36u32;
    let total_size = chunk_offset as usize + 8 + shader_bytes.len();
    let mut dxbc = Vec::with_capacity(total_size);
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // unknown
    dxbc.extend_from_slice(&(total_size as u32).to_le_bytes());
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk count
    dxbc.extend_from_slice(&chunk_offset.to_le_bytes());
    dxbc.extend_from_slice(b"SHDR");
    dxbc.extend_from_slice(&(shader_bytes.len() as u32).to_le_bytes());
    dxbc.extend_from_slice(&shader_bytes);

    let shader = D3d9Shader::parse(&dxbc).unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.instructions.len(), 2);
}

#[test]
fn malformed_truncated_instruction_errors() {
    let words = [0xFFFE_0200, 0x0200_0001, 0xC00F_0000];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 2,
            remaining_tokens: 1,
        }
    );
}

#[test]
fn malformed_unknown_opcode_errors() {
    // Unknown opcode with a valid (zero) operand length.
    let words = [0xFFFE_0200, 0x0000_7777, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::UnknownOpcode {
            opcode: 0x7777,
            specific: 0,
            at_token: 1,
        }
    );
}

#[test]
fn malformed_invalid_register_encoding_errors() {
    // mov <dst>, <src> with an invalid/unknown register type encoding in the dst token.
    //
    // Register type encoding is 5 bits split across the token:
    // - bits 28..=30
    // - bits 11..=12
    //
    // Here we encode 0b11111 (=31), which is not a valid D3D9 register type.
    let invalid_reg_token = 0xF00F_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0200_0001, // mov (len=2)
        invalid_reg_token,
        0x90E4_0000, // v0
        0x0000_FFFF, // end
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidRegisterEncoding {
            token: invalid_reg_token,
            at_token: 2,
        }
    );
}

#[test]
fn malformed_absurdly_large_instruction_length_errors() {
    // Comment instruction with an absurd length (15-bit length field set to max).
    // Ensure we fail with a structured error rather than panicking on bounds/allocations.
    let comment_token = 0x7FFF_FFFE; // opcode=0xfffe, length=0x7fff
    let words = [0xFFFE_0200, comment_token];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0xFFFE,
            at_token: 1,
            needed_tokens: 0x7FFF,
            remaining_tokens: 0,
        }
    );
}

#[test]
fn malformed_invalid_src_register_encoding_errors() {
    // mov <dst>, <src> with an invalid/unknown register type encoding in the src token.
    //
    // This exercises src-parameter decoding/validation (as opposed to dst-only validation).
    let invalid_src_token = 0xF0E4_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0200_0001, // mov (len=2)
        0x800F_0000, // r0
        invalid_src_token,
        0x0000_FFFF, // end
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidRegisterEncoding {
            token: invalid_src_token,
            at_token: 3,
        }
    );
}

#[test]
fn malformed_invalid_relative_register_encoding_errors() {
    // mov r0, c0[a?] with an invalid register type for the relative address register.
    //
    // Src token sets the relative-address flag, and the following token (the relative address
    // register) encodes an unknown register type.
    let relative_reg_token = 0xF000_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0300_0001, // mov (len=3)
        0x800F_0000, // r0
        0xA0E4_2000, // c0 (relative)
        relative_reg_token,
        0x0000_FFFF, // end
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidRegisterEncoding {
            token: relative_reg_token,
            at_token: 4,
        }
    );
}

#[test]
fn malformed_absurd_regular_instruction_length_errors() {
    // Non-comment instruction with a nonsensical length nibble (15) should not panic and should
    // fail with a structured truncation error.
    let words = [0xFFFE_0200, 0x0F00_0001, 0x800F_0000];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 15,
            remaining_tokens: 1,
        }
    );
}

#[test]
fn malformed_unknown_opcode_with_operands_errors() {
    // Unknown opcode with a non-zero operand length. Ensure we still surface the UnknownOpcode
    // error (and don't panic while reading the operand tokens).
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0200_7777, // unknown opcode 0x7777, len=2
        0x800F_0000, // r0 (arbitrary operand)
        0x90E4_0000, // v0 (arbitrary operand)
        0x0000_FFFF, // end
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::UnknownOpcode {
            opcode: 0x7777,
            specific: 0,
            at_token: 1,
        }
    );
}

#[test]
fn malformed_invalid_dcl_register_encoding_errors() {
    // dcl_* instruction with an invalid dst register encoding.
    let invalid_reg_token = 0xF00F_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0200_001F, // dcl (len=2)
        0x8000_0000, // usage token (position)
        invalid_reg_token,
        0x0000_FFFF, // end
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidRegisterEncoding {
            token: invalid_reg_token,
            at_token: 3,
        }
    );
}

#[test]
fn malformed_invalid_predicate_register_encoding_errors() {
    // Predicated instruction with an invalid predicate register encoding.
    let invalid_pred_token = 0xF000_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x1300_0001, // mov (len=3) + predicated flag
        invalid_pred_token,
        0x800F_0000, // r0
        0x90E4_0000, // v0
        0x0000_FFFF, // end
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidRegisterEncoding {
            token: invalid_pred_token,
            at_token: 2,
        }
    );
}

#[test]
fn malformed_truncated_comment_errors() {
    // Comment with length=1 but missing the payload token.
    let words = [0xFFFE_0200, 0x0001_FFFE];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0xFFFE,
            at_token: 1,
            needed_tokens: 1,
            remaining_tokens: 0,
        }
    );
}

#[test]
fn malformed_truncated_dcl_errors() {
    // dcl has a fixed operand length of 2 tokens; make the stream end after only 1 operand.
    let words = [0xFFFE_0200, 0x0200_001F, 0x8000_0000];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x001F,
            at_token: 1,
            needed_tokens: 2,
            remaining_tokens: 1,
        }
    );
}

#[test]
fn malformed_dcl_with_invalid_length_nibble_errors() {
    // dcl with an invalid length nibble (1) should still return a structured error and not panic
    // when attempting to access operands[1].
    let words = [0xFFFE_0200, 0x0100_001F, 0x8000_0000, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x001F,
            at_token: 1,
            needed_tokens: 2,
            remaining_tokens: 1,
        }
    );
}
