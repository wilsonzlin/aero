use aero_d3d9_shader::{D3d9Shader, Instruction, Opcode, ShaderStage};

fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
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
    let words = [0xFFFE_0200, 0x0200_0001];
    let err = D3d9Shader::parse(&words_to_bytes(&words)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("truncated instruction"));
}
