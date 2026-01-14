use aero_d3d9_shader::{D3d9Shader, Instruction, Opcode, ShaderParseError, ShaderStage};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};

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
    let dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"ISGN"), &[][..])]);

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert_eq!(err, ShaderParseError::DxbcMissingShaderChunk);
}

#[test]
fn rejects_oversized_bytecode() {
    // Ensure the debug/disassembler parser does not allocate unbounded memory on hostile input.
    let bytes = vec![0u8; 256 * 1024 + 4];
    let err = D3d9Shader::parse(&bytes).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::BytecodeTooLarge {
            len: 256 * 1024 + 4,
            max: 256 * 1024,
        }
    );
}

#[test]
fn malformed_dxbc_shader_chunk_invalid_byte_length_errors() {
    // DXBC container where the shader chunk payload isn't DWORD-aligned.
    let shader_bytes = vec![0u8; 5];
    let dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"SHDR"), shader_bytes.as_slice())]);

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert_eq!(err, ShaderParseError::InvalidByteLength { len: 5 });
}

#[test]
fn malformed_dxbc_shader_chunk_empty_errors() {
    // DXBC container with an empty shader chunk payload.
    let shader_bytes: [u8; 0] = [];
    let dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"SHDR"), shader_bytes.as_slice())]);

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert_eq!(err, ShaderParseError::Empty);
}

#[test]
fn malformed_dxbc_shader_chunk_too_large_errors() {
    // DXBC container where the shader chunk payload is larger than our parser cap.
    let shader_bytes = vec![0u8; 256 * 1024 + 4];
    let dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"SHDR"), shader_bytes.as_slice())]);

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::BytecodeTooLarge {
            len: 256 * 1024 + 4,
            max: 256 * 1024,
        }
    );
}

#[test]
fn malformed_dxbc_shader_chunk_invalid_version_token_errors() {
    // DXBC wrapper should propagate token-stream errors from inside the shader chunk.
    let shader_bytes = words_to_bytes(&[0x0001_0200, 0x0000_FFFF]);
    let dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"SHDR"), shader_bytes.as_slice())]);

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidVersionToken { token: 0x0001_0200 }
    );
}

#[test]
fn malformed_dxbc_truncated_header_errors() {
    // DXBC magic with no further header fields.
    let dxbc = dxbc_test_utils::build_container(&[]);
    let err = D3d9Shader::parse(&dxbc[..4]).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::MalformedHeader { .. })
    ));
}

#[test]
fn malformed_dxbc_total_size_out_of_bounds_errors() {
    // A minimal DXBC header whose declared `total_size` exceeds the provided buffer length.
    let mut dxbc = dxbc_test_utils::build_container(&[]);
    // total_size field is at offset 24.
    dxbc[24..28].copy_from_slice(&4096u32.to_le_bytes()); // total_size (too large)

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::OutOfBounds { .. })
    ));
}

#[test]
fn malformed_dxbc_total_size_too_small_errors() {
    // Declared total_size smaller than the DXBC header size should be rejected.
    let mut dxbc = dxbc_test_utils::build_container(&[]);
    // total_size field is at offset 24.
    dxbc[24..28].copy_from_slice(&16u32.to_le_bytes()); // total_size (too small)

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::MalformedHeader { .. })
    ));
}

#[test]
fn malformed_dxbc_chunk_count_too_large_errors() {
    // A minimal DXBC header with an absurd chunk_count should be rejected by aero-dxbc's bounds
    // checks (and surfaced through ShaderParseError).
    let mut dxbc = dxbc_test_utils::build_container(&[]);
    // chunk_count field is at offset 28.
    dxbc[28..32].copy_from_slice(&4097u32.to_le_bytes()); // chunk_count (exceeds MAX_DXBC_CHUNK_COUNT)

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::MalformedOffsets { .. })
    ));
}

#[test]
fn malformed_dxbc_offset_table_truncated_errors() {
    // Declared chunk_count requires an offset table entry, but total_size stops at the end of the
    // header (no offset table).
    let mut dxbc = dxbc_test_utils::build_container(&[]);
    // chunk_count field is at offset 28. Increase it without extending the offset table so the
    // parser sees a truncated offset table.
    dxbc[28..32].copy_from_slice(&1u32.to_le_bytes()); // chunk_count

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::MalformedOffsets { .. })
    ));
}

#[test]
fn malformed_dxbc_chunk_offset_into_header_errors() {
    // DXBC container with a chunk offset that points into the header should be rejected as a
    // malformed offset table.
    //
    // total_size: 36 bytes (header + 1 offset entry)
    // chunk_count: 1
    // chunk_offset: 0 (points into header)
    let mut dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"JUNK"), &[][..])]);
    // First (and only) chunk offset entry is at byte 32.
    dxbc[32..36].copy_from_slice(&0u32.to_le_bytes()); // chunk_offset[0] (points into header)

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::MalformedOffsets { .. })
    ));
}

#[test]
fn malformed_dxbc_chunk_offset_into_offset_table_errors() {
    // DXBC container with a chunk offset that points into the chunk offset table should be
    // rejected as a malformed offset table.
    //
    // total_size: 36 bytes (header + 1 offset entry)
    // chunk_count: 1
    // chunk_offset: 32 (DXBC_HEADER_LEN) -> inside the offset table
    let mut dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"JUNK"), &[][..])]);
    dxbc[32..36].copy_from_slice(&32u32.to_le_bytes()); // chunk_offset[0] (points into offset table)

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::MalformedOffsets { .. })
    ));
}

#[test]
fn malformed_dxbc_chunk_header_truncated_errors() {
    // DXBC container with a chunk offset pointing at the first byte after the offset table but
    // without enough remaining bytes for a full chunk header (8 bytes).
    let mut dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"JUNK"), &[][..])]);
    // total_size field is at offset 24. Shrink it so only 4 bytes remain after the offset table.
    dxbc[24..28].copy_from_slice(&40u32.to_le_bytes());

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::OutOfBounds { .. })
    ));
}

#[test]
fn malformed_dxbc_chunk_data_out_of_bounds_errors() {
    // DXBC container with a valid chunk header but a chunk size that would extend beyond the
    // declared total_size should be rejected as OutOfBounds.
    let mut dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"SHDR"), &[][..])]);
    // The first chunk header starts at offset 36; the size field is at offset 40.
    dxbc[40..44].copy_from_slice(&4u32.to_le_bytes()); // chunk size (would require 4 bytes of data)

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::OutOfBounds { .. })
    ));
}

#[test]
fn malformed_dxbc_chunk_offset_past_end_errors() {
    // Chunk offset is beyond the declared container bounds.
    let mut dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"JUNK"), &[][..])]);
    dxbc[32..36].copy_from_slice(&100u32.to_le_bytes()); // chunk_offset[0] (past end)

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::OutOfBounds { .. })
    ));
}

const VS_2_0_PASSTHROUGH: [u32; 14] = [
    0xFFFE_0200, // vs_2_0
    // dcl_position v0
    0x0300_001F,
    0x8000_0000,
    0x900F_0000,
    // dcl_texcoord0 v1
    0x0300_001F,
    0x8000_0005,
    0x900F_0001,
    // mov oPos, v0
    0x0300_0001,
    0xC00F_0000,
    0x90E4_0000,
    // mov oT0, v1
    0x0300_0001,
    0xE00F_0000,
    0x90E4_0001,
    // end
    0x0000_FFFF,
];

const VS_2_0_PASSTHROUGH_OPERAND_COUNT_LEN: [u32; 14] = [
    0xFFFE_0200, // vs_2_0
    // dcl_position v0 (length nibble encodes operand count)
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
    0x0300_001F,
    0x8000_0005,
    0x900F_0000,
    // dcl_2d s0
    0x0300_001F,
    0x9000_0000,
    0xA00F_0800,
    // texld r0, v0, s0
    0x0400_0042,
    0x800F_0000,
    0x90E4_0000,
    0xA0E4_0800,
    // mov oC0, r0
    0x0300_0001,
    0x800F_0800,
    0x80E4_0000,
    // end
    0x0000_FFFF,
];

const PS_2_0_LRP: [u32; 19] = [
    0xFFFF_0200, // ps_2_0
    // dcl_texcoord0 v0
    0x0300_001F,
    0x8000_0005,
    0x900F_0000,
    // dcl_texcoord1 v1
    0x0300_001F,
    0x8001_0005,
    0x900F_0001,
    // dcl_texcoord2 v2
    0x0300_001F,
    0x8002_0005,
    0x900F_0002,
    // lrp r0, v0, v1, v2
    0x0500_0012,
    0x800F_0000,
    0x90E4_0000,
    0x90E4_0001,
    0x90E4_0002,
    // mov oC0, r0
    0x0300_0001,
    0x800F_0800,
    0x80E4_0000,
    // end
    0x0000_FFFF,
];

const VS_3_0_IF: [u32; 30] = [
    0xFFFE_0300, // vs_3_0
    // dcl_position v0
    0x0300_001F,
    0x8000_0000,
    0x900F_0000,
    // dcl_position o0
    0x0300_001F,
    0x8000_0000,
    0xE00F_0000,
    // dcl_texcoord0 v1
    0x0300_001F,
    0x8000_0005,
    0x900F_0001,
    // dcl_texcoord0 o1
    0x0300_001F,
    0x8000_0005,
    0xE00F_0001,
    // mov o0, v0
    0x0300_0001,
    0xE00F_0000,
    0x90E4_0000,
    // mov o1, v1
    0x0300_0001,
    0xE00F_0001,
    0x90E4_0001,
    // setp p0.x, v0.x, c0.x
    0x0400_005E,
    0xB001_1000,
    0x9000_0000,
    0xA000_0000,
    // if p0.x
    0x0200_0028,
    0xB000_1000,
    // mov o1, v1
    0x0300_0001,
    0xE00F_0001,
    0x90E4_0001,
    // endif
    0x0100_002B,
    // end
    0x0000_FFFF,
];

const PS_3_0_TEXKILL: [u32; 17] = [
    0xFFFF_0300, // ps_3_0
    // dcl_texcoord0 v0
    0x0300_001F,
    0x8000_0005,
    0x900F_0000,
    // dcl_2d s0
    0x0300_001F,
    0x9000_0000,
    0xA00F_0800,
    // texld r0, v0, s0
    0x0400_0042,
    0x800F_0000,
    0x90E4_0000,
    0xA0E4_0800,
    // texkill r0
    0x0200_0041,
    0x80E4_0000,
    // mov oC0, r0
    0x0300_0001,
    0x800F_0800,
    0x80E4_0000,
    // end
    0x0000_FFFF,
];

const PS_3_0_DSX_DSY: [u32; 18] = [
    0xFFFF_0300, // ps_3_0
    // dcl_texcoord0 v0
    0x0300_001F,
    0x8000_0005,
    0x900F_0000,
    // dsx r0, v0
    0x0300_0056,
    0x800F_0000,
    0x90E4_0000,
    // dsy r1, v0
    0x0300_0057,
    0x800F_0001,
    0x90E4_0000,
    // add r0, r0, r1
    0x0400_0002,
    0x800F_0000,
    0x80E4_0000,
    0x80E4_0001,
    // mov oC0, r0
    0x0300_0001,
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
fn parse_vs_2_0_passthrough_operand_count_length_encoding() {
    let shader = D3d9Shader::parse(&words_to_bytes(&VS_2_0_PASSTHROUGH_OPERAND_COUNT_LEN)).unwrap();
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
fn parse_ps_2_0_lrp() {
    let shader = D3d9Shader::parse(&words_to_bytes(&PS_2_0_LRP)).unwrap();
    assert_eq!(shader.stage, ShaderStage::Pixel);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.declarations.len(), 3);
    assert_eq!(shader.instructions.len(), 2);
    assert!(matches!(
        shader.instructions[0],
        Instruction::Op {
            opcode: Opcode::Lrp,
            ..
        }
    ));

    let dis = shader.disassemble();
    assert!(dis.contains("ps_2_0"));
    assert!(dis.contains("lrp r0, v0, v1, v2"));
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
fn parse_ps_3_0_derivatives() {
    let shader = D3d9Shader::parse(&words_to_bytes(&PS_3_0_DSX_DSY)).unwrap();
    assert_eq!(shader.stage, ShaderStage::Pixel);
    assert_eq!(shader.model.major, 3);
    assert!(matches!(
        shader.instructions[0],
        Instruction::Op {
            opcode: Opcode::Dsx,
            ..
        }
    ));
    assert!(matches!(
        shader.instructions[1],
        Instruction::Op {
            opcode: Opcode::Dsy,
            ..
        }
    ));

    let dis = shader.disassemble();
    assert!(dis.contains("dsx r0, v0"));
    assert!(dis.contains("dsy r1, v0"));
}

#[test]
fn parse_dxbc_wrapped_sm2() {
    let shader_bytes = words_to_bytes(&VS_2_0_PASSTHROUGH);
    let dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"SHDR"), shader_bytes.as_slice())]);

    let shader = D3d9Shader::parse(&dxbc).unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.instructions.len(), 2);
}

#[test]
fn parse_dxbc_wrapped_shex_sm2() {
    // Some toolchains wrap bytecode in a `SHEX` chunk instead of `SHDR`.
    let shader_bytes = words_to_bytes(&VS_2_0_PASSTHROUGH);
    let dxbc = dxbc_test_utils::build_container(&[(FourCC(*b"SHEX"), shader_bytes.as_slice())]);

    let shader = D3d9Shader::parse(&dxbc).unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.instructions.len(), 2);
}

#[test]
fn parse_dxbc_skips_non_shader_chunks() {
    // Ensure the parser finds `SHDR` even if other chunks come first.
    let shader_bytes = words_to_bytes(&VS_2_0_PASSTHROUGH);
    let dxbc = dxbc_test_utils::build_container(&[
        (FourCC(*b"ISGN"), &[]),
        (FourCC(*b"SHDR"), shader_bytes.as_slice()),
    ]);

    let shader = D3d9Shader::parse(&dxbc).unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.instructions.len(), 2);
}

#[test]
fn malformed_truncated_instruction_errors() {
    let words = [0xFFFE_0200, 0x0300_0001, 0xC00F_0000];
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
fn malformed_unknown_opcode_specific_field_errors() {
    // Unknown opcode should preserve the opcode-specific field (bits 16..24) in the error.
    let words = [0xFFFE_0200, 0x00AB_7777, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::UnknownOpcode {
            opcode: 0x7777,
            specific: 0xAB,
            at_token: 1,
        }
    );
}

#[test]
fn malformed_truncated_unknown_opcode_errors() {
    // Unknown opcode with a non-zero operand length, but a truncated operand stream.
    let words = [0xFFFE_0200, 0x0300_7777, 0x800F_0000];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x7777,
            at_token: 1,
            needed_tokens: 2,
            remaining_tokens: 1,
        }
    );
}

#[test]
fn malformed_predicated_missing_predicate_token_errors() {
    // Predicated instruction with no operands should be rejected (predicate token is missing).
    let words = [0xFFFE_0200, 0x1000_0001, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 1,
            remaining_tokens: 0,
        }
    );
}

#[test]
fn malformed_predicated_relative_predicate_token_errors() {
    // Predicate token uses the same encoding as a source parameter and may technically set the
    // relative-addressing bit. However, the predicate is always the *last* operand token, so a
    // relative predicate encoding cannot provide the required extra token.
    let words = [
        0xFFFE_0200,
        0x1300_0001, // mov len=3 + predicated
        0x800F_0000, // r0
        0xB000_3000, // p0.x with RELATIVE bit set (missing relative token)
        0x0000_FFFF,
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 3,
            remaining_tokens: 2,
        }
    );
}

#[test]
fn malformed_predicated_relative_predicate_without_param_bit_errors() {
    // Same as `malformed_predicated_relative_predicate_token_errors`, but with bit31 cleared on the
    // predicate token. Some real-world D3D9 encodings omit bit31 on parameter tokens, and we want
    // to ensure we still treat a relative predicate as malformed (since the predicate is always
    // the final operand token).
    let words = [
        0xFFFE_0200,
        0x1300_0001, // mov len=3 + predicated
        0x800F_0000, // r0
        0x0000_2000, // (relative) but missing the relative register token
        0x0000_FFFF,
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 3,
            remaining_tokens: 2,
        }
    );
}

#[test]
fn malformed_mov_missing_dst_token_errors() {
    // mov with an empty operand list should be rejected.
    let words = [0xFFFE_0200, 0x0000_0001, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 1,
            remaining_tokens: 0,
        }
    );
}

#[test]
fn malformed_mov_missing_src_token_errors() {
    // mov with only a dst token should be rejected (needs at least one src token).
    let words = [0xFFFE_0200, 0x0200_0001, 0x800F_0000, 0x0000_FFFF];
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
fn malformed_relative_src_missing_token_errors() {
    // Source parameter requests relative addressing but the relative register token is missing.
    //
    // The opcode length field is (malformed) too small to include the required extra token.
    let words = [
        0xFFFE_0200,
        0x0300_0001, // mov len=3 (opcode + dst + src)
        0x800F_0000, // r0
        0xA0E4_2000, // c0 (relative) - missing relative register token
        0x0000_FFFF,
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 3,
            remaining_tokens: 2,
        }
    );
}

#[test]
fn malformed_relative_src_missing_token_without_param_bit_errors() {
    // Source token sets the relative-address bit but does not have bit31 set. This is not a valid
    // D3D9 encoding, but the parser should still treat it as malformed and avoid silently dropping
    // relative addressing.
    let words = [
        0xFFFE_0200,
        0x0300_0001, // mov len=3 (opcode + dst + src)
        0x800F_0000, // r0
        0x0000_2000, // r0 (relative) but missing relative register token
        0x0000_FFFF,
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 3,
            remaining_tokens: 2,
        }
    );
}

#[test]
fn malformed_predicated_mov_missing_src_token_errors() {
    // Predicated mov with a predicate + dst but missing a src token.
    let words = [
        0xFFFE_0200,
        0x1300_0001, // mov len=3 + predicated
        0x800F_0000, // r0
        0xB000_1000, // p0.x
        0x0000_FFFF,
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 3,
            remaining_tokens: 2,
        }
    );
}

#[test]
fn malformed_predicated_relative_src_missing_token_errors() {
    // Predicated instruction where a source parameter requests relative addressing, but the length
    // nibble is too small to include the required extra token (predicate must remain last).
    let words = [
        0xFFFE_0200,
        0x1400_0001, // mov len=4 + predicated
        0x800F_0000, // r0 (dst)
        0xA0E4_2000, // c0 (src, relative) - missing relative register token
        0xB000_1000, // p0.x (predicate)
        0x0000_FFFF,
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0001,
            at_token: 1,
            needed_tokens: 4,
            remaining_tokens: 3,
        }
    );
}

#[test]
fn malformed_if_missing_src_token_errors() {
    // `if` requires a condition source operand.
    let words = [0xFFFE_0300, 0x0000_0028, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0028,
            at_token: 1,
            needed_tokens: 1,
            remaining_tokens: 0,
        }
    );
}

#[test]
fn malformed_texkill_missing_src_token_errors() {
    // `texkill` requires a source operand.
    let words = [0xFFFF_0200, 0x0000_0041, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x0041,
            at_token: 1,
            needed_tokens: 1,
            remaining_tokens: 0,
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
        0x0300_0001, // mov (len=3)
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
fn malformed_invalid_register_encoding_without_param_bit_errors() {
    // Same as `malformed_invalid_register_encoding_errors`, but with bit31 cleared on the invalid
    // destination token. Some toolchains omit bit31 on parameter tokens; ensure invalid register
    // type encodings are still rejected (and don't panic or get mis-decoded as immediates).
    let invalid_reg_token = 0x700F_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0300_0001, // mov (len=3)
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
        0x0300_0001, // mov (len=3)
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
        0x0400_0001, // mov (len=4)
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
            needed_tokens: 14,
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
        0x0300_7777, // unknown opcode 0x7777, len=3
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
fn malformed_unknown_opcode_specific_bits_errors() {
    // Unknown opcode with a non-zero `specific` field. Ensure the parser preserves the decoded
    // `specific` bits in the error (stable error variant) and does not attempt to interpret the
    // instruction as a known opcode.
    let opcode_token = 0x01AB_7777; // len=1, specific=0xAB, opcode=0x7777
    let words = [
        0xFFFE_0200, // vs_2_0
        opcode_token,
        0x0000_FFFF, // end (should not be reached)
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::UnknownOpcode {
            opcode: 0x7777,
            specific: 0xAB,
            at_token: 1,
        }
    );
}

#[test]
fn malformed_unknown_opcode_truncated_operand_stream_errors() {
    // Unknown opcode whose length nibble demands more operand tokens than are present. This should
    // be treated as a structured truncation error (not a panic) before we even reach the opcode
    // decode step.
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0400_1234, // unknown opcode 0x1234, len=4 => operand_len=3
        0x800F_0000, // r0 (only 1 operand token present; 2 missing)
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x1234,
            at_token: 1,
            needed_tokens: 3,
            remaining_tokens: 1,
        }
    );
}

#[test]
fn malformed_invalid_dcl_register_encoding_errors() {
    // dcl_* instruction with an invalid dst register encoding.
    let invalid_reg_token = 0xF00F_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0300_001F, // dcl (len=3)
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
fn malformed_invalid_dcl_register_encoding_modern_form_errors() {
    // Modern `dcl` encodings may only include the destination register token, with usage/texture
    // information encoded in the opcode token itself. Ensure invalid register encodings are still
    // rejected in this form.
    let invalid_reg_token = 0xF00F_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x0200_001F, // dcl (len=2)
        invalid_reg_token,
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
fn malformed_invalid_predicate_register_encoding_errors() {
    // Predicated instruction with an invalid predicate register encoding.
    let invalid_pred_token = 0xF000_1800;
    let words = [
        0xFFFE_0200, // vs_2_0
        0x1400_0001, // mov (len=4) + predicated flag
        0x800F_0000, // r0
        0x90E4_0000, // v0
        invalid_pred_token,
        0x0000_FFFF, // end
    ];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::InvalidRegisterEncoding {
            token: invalid_pred_token,
            at_token: 4,
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
    let words = [0xFFFE_0200, 0x0300_001F, 0x8000_0000];
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
    // dcl with a length nibble that yields zero operands should still return a structured error
    // and not panic when attempting to decode operand tokens.
    let words = [0xFFFE_0200, 0x0100_001F, 0x8000_0000, 0x0000_FFFF];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    assert_eq!(
        err,
        ShaderParseError::TruncatedInstruction {
            opcode: 0x001F,
            at_token: 1,
            needed_tokens: 1,
            remaining_tokens: 0,
        }
    );
}

#[test]
fn parse_real_fixture_ps_2_0_sample() {
    let shader = D3d9Shader::parse(include_bytes!(
        "../../../aero-d3d9/tests/fixtures/dxbc/ps_2_0_sample.dxbc"
    ))
    .unwrap();
    assert_eq!(shader.stage, ShaderStage::Pixel);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.model.minor, 0);
    assert_eq!(shader.declarations.len(), 1);
    assert_eq!(shader.instructions.len(), 3);
    let dis = shader.disassemble();
    assert!(dis.contains("ps_2_0"));
    // Ensure we can decode fxc-style `t#` register tokens (bit31 is not required).
    assert!(dis.contains("texld r0, t0, s0"));
    assert!(dis.contains("mov oC0, r0"));
}

#[test]
fn parse_real_fixture_ps_3_0_math() {
    let shader = D3d9Shader::parse(include_bytes!(
        "../../../aero-d3d9/tests/fixtures/dxbc/ps_3_0_math.dxbc"
    ))
    .unwrap();
    assert_eq!(shader.stage, ShaderStage::Pixel);
    assert_eq!(shader.model.major, 3);
    assert_eq!(shader.model.minor, 0);
    assert_eq!(shader.declarations.len(), 1);
    assert_eq!(shader.instructions.len(), 4);
    let dis = shader.disassemble();
    assert!(dis.contains("ps_3_0"));
    assert!(dis.contains("dp3 r0, t0, c0"));
    assert!(dis.contains("rsq r1, r0"));
    assert!(dis.contains("mov oC0, r0"));
}

#[test]
fn parse_real_fixture_vs_2_0_simple() {
    let shader = D3d9Shader::parse(include_bytes!(
        "../../../aero-d3d9/tests/fixtures/dxbc/vs_2_0_simple.dxbc"
    ))
    .unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 2);
    assert_eq!(shader.model.minor, 0);
    assert_eq!(shader.declarations.len(), 1);
    assert_eq!(shader.instructions.len(), 2);
    let dis = shader.disassemble();
    assert!(dis.contains("vs_2_0"));
    assert!(dis.contains("m4x4 oPos, v0, c0"));
    assert!(dis.contains("mov oT0, v1"));
}

#[test]
fn parse_real_fixture_vs_3_0_branch() {
    let shader = D3d9Shader::parse(include_bytes!(
        "../../../aero-d3d9/tests/fixtures/dxbc/vs_3_0_branch.dxbc"
    ))
    .unwrap();
    assert_eq!(shader.stage, ShaderStage::Vertex);
    assert_eq!(shader.model.major, 3);
    assert_eq!(shader.model.minor, 0);
    assert_eq!(shader.declarations.len(), 1);
    assert_eq!(shader.instructions.len(), 5);
    let dis = shader.disassemble();
    assert!(dis.contains("vs_3_0"));
    assert!(dis.contains("if b0"));
    assert!(dis.contains("endif"));
    assert!(dis.contains("mov oPos, r0"));
}
