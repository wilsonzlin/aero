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
    let err = D3d9Shader::parse(b"DXBC").unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::MalformedHeader { .. })
    ));
}

#[test]
fn malformed_dxbc_total_size_out_of_bounds_errors() {
    // A minimal DXBC header whose declared `total_size` exceeds the provided buffer length.
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&4096u32.to_le_bytes()); // total_size (too large)
    dxbc.extend_from_slice(&0u32.to_le_bytes()); // chunk_count

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::OutOfBounds { .. })
    ));
}

#[test]
fn malformed_dxbc_total_size_too_small_errors() {
    // Declared total_size smaller than the DXBC header size should be rejected.
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&16u32.to_le_bytes()); // total_size (too small)
    dxbc.extend_from_slice(&0u32.to_le_bytes()); // chunk_count

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
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&32u32.to_le_bytes()); // total_size
    dxbc.extend_from_slice(&4097u32.to_le_bytes()); // chunk_count (exceeds MAX_DXBC_CHUNK_COUNT)

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
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&32u32.to_le_bytes()); // total_size (header only)
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk_count

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
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&36u32.to_le_bytes()); // total_size
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk_count
    dxbc.extend_from_slice(&0u32.to_le_bytes()); // chunk_offset[0]

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
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&36u32.to_le_bytes()); // total_size
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk_count
    dxbc.extend_from_slice(&32u32.to_le_bytes()); // chunk_offset[0]

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
    let chunk_offset = 36u32;
    let total_size = 40u32; // only 4 bytes after offset table
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&total_size.to_le_bytes());
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk_count
    dxbc.extend_from_slice(&chunk_offset.to_le_bytes());
    dxbc.resize(total_size as usize, 0);

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
    let chunk_offset = 36u32;
    let total_size = 44u32; // enough for header + offset table + chunk header (8 bytes), but no data
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&total_size.to_le_bytes());
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk_count
    dxbc.extend_from_slice(&chunk_offset.to_le_bytes());
    dxbc.extend_from_slice(b"SHDR");
    dxbc.extend_from_slice(&4u32.to_le_bytes()); // chunk size (would require 4 bytes of data)
    dxbc.resize(total_size as usize, 0);

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::OutOfBounds { .. })
    ));
}

#[test]
fn malformed_dxbc_chunk_offset_past_end_errors() {
    // Chunk offset is beyond the declared container bounds.
    let total_size = 36u32; // header + offset table (1 entry)
    let mut dxbc = Vec::new();
    dxbc.extend_from_slice(b"DXBC");
    dxbc.extend_from_slice(&[0u8; 16]); // checksum
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // reserved
    dxbc.extend_from_slice(&total_size.to_le_bytes());
    dxbc.extend_from_slice(&1u32.to_le_bytes()); // chunk_count
    dxbc.extend_from_slice(&100u32.to_le_bytes()); // chunk_offset[0] (past end)
    dxbc.resize(total_size as usize, 0);

    let err = D3d9Shader::parse(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        ShaderParseError::Dxbc(aero_dxbc::DxbcError::OutOfBounds { .. })
    ));
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

const PS_2_0_LRP: [u32; 19] = [
    0xFFFF_0200, // ps_2_0
    // dcl_texcoord0 v0
    0x0200_001F,
    0x8000_0005,
    0x900F_0000,
    // dcl_texcoord1 v1
    0x0200_001F,
    0x8001_0005,
    0x900F_0001,
    // dcl_texcoord2 v2
    0x0200_001F,
    0x8002_0005,
    0x900F_0002,
    // lrp r0, v0, v1, v2
    0x0400_0012,
    0x800F_0000,
    0x90E4_0000,
    0x90E4_0001,
    0x90E4_0002,
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
    let words = [0xFFFE_0200, 0x0200_7777, 0x800F_0000];
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
    let words = [0xFFFE_0200, 0x0100_0001, 0x800F_0000, 0x0000_FFFF];
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
fn malformed_predicated_mov_missing_src_token_errors() {
    // Predicated mov with a predicate + dst but missing a src token.
    let words = [
        0xFFFE_0200,
        0x1200_0001, // mov len=2 + predicated
        0xB000_1000, // p0.x
        0x800F_0000, // r0
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
