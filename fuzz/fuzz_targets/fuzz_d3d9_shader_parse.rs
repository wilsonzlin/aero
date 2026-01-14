#![no_main]

use libfuzzer_sys::fuzz_target;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};

/// Hard cap on the raw libFuzzer input size. This limits allocations in both the D3D9 shader token
/// parser and the optional DXBC wrapper parser.
const MAX_INPUT_SIZE_BYTES: usize = 256 * 1024; // 256 KiB

/// Cap the token-stream bytes we feed into `aero_d3d9_shader` to keep parsing time predictable.
/// D3D9 shader blobs are typically small, and we mainly want to stress bounds checks / decoding.
const MAX_TOKEN_STREAM_BYTES: usize = 64 * 1024; // 64 KiB

/// Cap the amount of work done by the disassembler (string formatting can otherwise be quadratic
/// in the worst case for huge instruction streams).
const MAX_DECLARATIONS: usize = 512;
const MAX_INSTRUCTIONS: usize = 2048;
const SYNTH_MAX_INSTRUCTIONS: usize = 16;

fn wrap_dxbc(shader_bytes: &[u8], chunk_fourcc: [u8; 4]) -> Vec<u8> {
    dxbc_test_utils::build_container(&[(FourCC(chunk_fourcc), shader_bytes)])
}

fn disassemble_bounded(mut shader: aero_d3d9_shader::D3d9Shader) {
    shader.declarations.truncate(MAX_DECLARATIONS);
    shader.instructions.truncate(MAX_INSTRUCTIONS);
    let dis = shader.disassemble();
    // Touch the result to keep the optimizer from discarding formatting work.
    let _ = dis.len();
}

fn patched_token_stream(data: &[u8]) -> Vec<u8> {
    // Copy a bounded number of bytes and align to 32-bit tokens.
    let mut out = data[..data.len().min(MAX_TOKEN_STREAM_BYTES)].to_vec();
    out.truncate((out.len() / 4) * 4);
    if out.len() < 4 {
        out.resize(4, 0);
    }

    // Force a valid D3D9 version token (vs/ps + {1,2,3}_x) so we reach deeper decode paths more
    // often than relying on pure random chance.
    let b0 = data.get(0).copied().unwrap_or(0);
    let b1 = data.get(1).copied().unwrap_or(0);
    let b2 = data.get(2).copied().unwrap_or(0);
    let shader_type: u16 = if (b0 & 1) == 0 { 0xFFFE } else { 0xFFFF }; // vs / ps
    let major: u8 = (b1 % 3) + 1;
    let minor: u8 = b2 % 4;
    let version_token: u32 = ((shader_type as u32) << 16) | ((major as u32) << 8) | minor as u32;
    out[0..4].copy_from_slice(&version_token.to_le_bytes());

    // Encourage termination: place an END token at the end of the buffer when possible.
    if out.len() >= 8 {
        let end = out.len();
        out[end - 4..end].copy_from_slice(&0x0000_FFFFu32.to_le_bytes());
    }

    out
}

fn encode_reg_type_bits(ty_raw: u8) -> u32 {
    // D3D9 register types are split across:
    // - bits 28..30 (low 3 bits), and
    // - bits 11..12 (high 2 bits).
    let ty = u32::from(ty_raw);
    ((ty & 0x7) << 28) | ((ty & 0x18) << 8)
}

fn encode_register_token(ty_raw: u8, reg_num: u16) -> u32 {
    (u32::from(reg_num) & 0x0000_07FF) | encode_reg_type_bits(ty_raw)
}

fn encode_dst_token(ty_raw: u8, reg_num: u16, write_mask: u8) -> u32 {
    encode_register_token(ty_raw, reg_num) | (u32::from(write_mask & 0xF) << 16)
}

fn encode_src_token(ty_raw: u8, reg_num: u16, swizzle: u8, modifier: u8) -> u32 {
    encode_register_token(ty_raw, reg_num)
        | (u32::from(swizzle) << 16)
        | (u32::from(modifier & 0xF) << 24)
}

/// Build a small, mostly-valid token stream that parses successfully and exercises the disassembler
/// more reliably than fully-random streams.
fn synth_token_stream(data: &[u8]) -> Vec<u8> {
    let mut idx = 0usize;
    let mut next_u8 = || {
        let b = data.get(idx).copied().unwrap_or(0);
        idx = idx.saturating_add(1);
        b
    };

    let b0 = next_u8();
    let b1 = next_u8();
    let b2 = next_u8();

    let is_pixel = (b0 & 1) != 0;
    let shader_type: u16 = if is_pixel { 0xFFFF } else { 0xFFFE }; // ps / vs
    let major: u8 = (b1 % 3) + 1;
    let minor: u8 = b2 % 4;
    let version_token: u32 = ((shader_type as u32) << 16) | ((major as u32) << 8) | minor as u32;

    let mut tokens: Vec<u32> = Vec::new();
    tokens.push(version_token);

    let inst_count = (next_u8() as usize) % (SYNTH_MAX_INSTRUCTIONS + 1);

    for _ in 0..inst_count {
        match next_u8() % 9 {
            // NOP: may still carry some src operands (not semantically meaningful, but valid).
            0 => {
                let src_count = (next_u8() % 3) as usize;
                let len = 1 + src_count;
                tokens.push(0u32 | ((len as u32) << 24));
                for _ in 0..src_count {
                    let ty = next_u8() % 20;
                    let reg = u16::from(next_u8());
                    let swz = next_u8();
                    let mod_raw = next_u8() & 0xF;
                    tokens.push(encode_src_token(ty, reg, swz, mod_raw));
                }
            }
            // MOV: dst + 1..3 src
            1 => {
                let src_count = 1 + (next_u8() % 3) as usize;
                let len = 2 + src_count;
                tokens.push(1u32 | ((len as u32) << 24));
                let dst_reg = u16::from(next_u8());
                let write_mask = (next_u8() & 0xF).max(1);
                tokens.push(encode_dst_token(0, dst_reg, write_mask));
                for _ in 0..src_count {
                    let ty = next_u8() % 20;
                    let reg = u16::from(next_u8());
                    let swz = next_u8();
                    let mod_raw = next_u8() & 0xF;
                    tokens.push(encode_src_token(ty, reg, swz, mod_raw));
                }
            }
            // ADD: dst + 1..3 src
            2 => {
                let src_count = 1 + (next_u8() % 3) as usize;
                let len = 2 + src_count;
                tokens.push(2u32 | ((len as u32) << 24));
                let dst_reg = u16::from(next_u8());
                let write_mask = (next_u8() & 0xF).max(1);
                tokens.push(encode_dst_token(0, dst_reg, write_mask));
                for _ in 0..src_count {
                    let ty = next_u8() % 20;
                    let reg = u16::from(next_u8());
                    let swz = next_u8();
                    let mod_raw = next_u8() & 0xF;
                    tokens.push(encode_src_token(ty, reg, swz, mod_raw));
                }
            }
            // MAD: dst + 1..3 src
            3 => {
                let src_count = 1 + (next_u8() % 3) as usize;
                let len = 2 + src_count;
                tokens.push(4u32 | ((len as u32) << 24));
                let dst_reg = u16::from(next_u8());
                let write_mask = (next_u8() & 0xF).max(1);
                tokens.push(encode_dst_token(0, dst_reg, write_mask));
                for _ in 0..src_count {
                    let ty = next_u8() % 20;
                    let reg = u16::from(next_u8());
                    let swz = next_u8();
                    let mod_raw = next_u8() & 0xF;
                    tokens.push(encode_src_token(ty, reg, swz, mod_raw));
                }
            }
            // IF: one src operand.
            4 => {
                tokens.push(40u32 | (2u32 << 24));
                let ty = next_u8() % 20;
                let reg = u16::from(next_u8());
                let swz = next_u8();
                let mod_raw = next_u8() & 0xF;
                tokens.push(encode_src_token(ty, reg, swz, mod_raw));
            }
            // TEXKILL: one src operand.
            5 => {
                tokens.push(65u32 | (2u32 << 24));
                let ty = next_u8() % 20;
                let reg = u16::from(next_u8());
                let swz = next_u8();
                let mod_raw = next_u8() & 0xF;
                tokens.push(encode_src_token(ty, reg, swz, mod_raw));
            }
            // DCL (sampler or semantic declaration).
            6 => {
                let sampler = (next_u8() & 1) == 0;
                if sampler {
                    let texture_type = u32::from(next_u8() & 0xF);
                    let sampler_reg = u16::from(next_u8() % 16);
                    let opcode_token = 31u32 | (2u32 << 24) | (texture_type << 16);
                    tokens.push(opcode_token);
                    tokens.push(encode_dst_token(10, sampler_reg, 0xF));
                } else {
                    let usage_raw = if (next_u8() & 1) == 0 { 5u32 } else { 10u32 }; // texcoord/color
                    let usage_index = u32::from(next_u8() & 0xF);
                    let opcode_token = 31u32
                        | (2u32 << 24)
                        | (usage_raw << 16)
                        | (usage_index << 20);
                    tokens.push(opcode_token);
                    let reg_ty = if is_pixel { 3u8 } else { 1u8 }; // t# (ps) or v# (vs)
                    let reg_num = u16::from(next_u8() % 16);
                    tokens.push(encode_dst_token(reg_ty, reg_num, 0xF));
                }
            }
            // COMMENT block (length in DWORDs lives in bits 16..30).
            7 => {
                let comment_len = u32::from(next_u8() % 4);
                tokens.push(0xFFFEu32 | (comment_len << 16));
                for _ in 0..comment_len {
                    let w = u32::from_le_bytes([next_u8(), next_u8(), next_u8(), next_u8()]);
                    tokens.push(w);
                }
            }
            // RET
            _ => {
                tokens.push(28u32 | (1u32 << 24));
            }
        }
    }

    // END token.
    tokens.push(0x0000_FFFF);

    let mut out = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // 1) Parse the bytes as-is. This will exercise the DXBC extraction path when `data` happens to
    // start with "DXBC", and will otherwise attempt to treat the input as a raw DWORD token
    // stream.
    // Note: we additionally cap the bytes passed to the parser to keep instruction iteration
    // bounded even when libFuzzer is configured with a very large `-max_len`.
    let bounded = &data[..data.len().min(MAX_TOKEN_STREAM_BYTES)];
    if let Ok(shader) = aero_d3d9_shader::D3d9Shader::parse(bounded) {
        disassemble_bounded(shader);
    }

    // 2) Also patch the first token to a valid shader version token and ensure the input is
    // DWORD-aligned so the token-stream parser gets more coverage.
    let patched = patched_token_stream(data);
    if let Ok(shader) = aero_d3d9_shader::D3d9Shader::parse(&patched) {
        disassemble_bounded(shader);
    }

    // 3) Synthesize a small, mostly-valid token stream. This increases the odds of a successful
    // parse, which in turn stresses the disassembler formatting paths.
    let synth = synth_token_stream(data);
    if let Ok(shader) = aero_d3d9_shader::D3d9Shader::parse(&synth) {
        disassemble_bounded(shader);
    }

    // 4) Wrap a token stream in a minimal valid DXBC container. This reliably exercises the DXBC
    // extraction path even when the raw fuzz input does not start with "DXBC".
    let chunk = if data.get(0).copied().unwrap_or(0) & 1 == 0 {
        *b"SHDR"
    } else {
        *b"SHEX"
    };
    let dxbc = wrap_dxbc(&synth, chunk);
    if let Ok(shader) = aero_d3d9_shader::D3d9Shader::parse(&dxbc) {
        disassemble_bounded(shader);
    }
});
