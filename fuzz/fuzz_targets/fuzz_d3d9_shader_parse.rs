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

const OPCODE_COMMENT: u32 = 0xFFFE;
const OPCODE_END: u32 = 0xFFFF;
const OPCODE_DCL: u32 = 31;

const OPCODE_IF: u32 = 40;
const OPCODE_ELSE: u32 = 42;
const OPCODE_ENDIF: u32 = 43;
const OPCODE_BREAKC: u32 = 45;
const OPCODE_RET: u32 = 28;
const OPCODE_TEXKILL: u32 = 65;
const OPCODE_TEXLD: u32 = 66;
const OPCODE_DP2ADD: u32 = 89;
const OPCODE_SETP: u32 = 94;

const PREDICATED_BIT: u32 = 0x1000_0000;
const COISSUE_BIT: u32 = 0x4000_0000;
const ADDR_MODE_RELATIVE_BIT: u32 = 0x0000_2000;

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

fn synth_opcode_token(opcode_raw: u32, len: usize) -> u32 {
    // D3D9 encodes the total instruction length (in u32 tokens) in bits 24..27. Values outside the
    // nibble are not representable; keep synthesis bounded so we never exceed 15.
    let len = (len & 0xF) as u32;
    opcode_raw | (len << 24)
}

fn synth_predicate_token(next_u8: &mut impl FnMut() -> u8) -> u32 {
    // Predicate register token: p0..p3, no relative addressing.
    let reg = u16::from(next_u8() % 4);
    let swz = next_u8();
    encode_src_token(19, reg, swz, 0)
}

fn synth_push_src_tokens(
    next_u8: &mut impl FnMut() -> u8,
    operands: &mut Vec<u32>,
    allow_relative: bool,
) {
    // For relative addressing, prefer constant registers so the disassembly prints in the common
    // `c[a0.x+N]` style.
    let make_relative = allow_relative && (next_u8() & 7) == 0;
    let ty = if make_relative { 2u8 } else { next_u8() % 20 };
    let reg = u16::from(next_u8());
    let swz = next_u8();
    let mod_raw = next_u8() & 0xF;
    let mut token = encode_src_token(ty, reg, swz, mod_raw);
    if make_relative {
        token |= ADDR_MODE_RELATIVE_BIT;
        operands.push(token);

        let rel_reg = u16::from(next_u8() % 2); // a0/a1
        let rel_swz = next_u8();
        operands.push(encode_src_token(3, rel_reg, rel_swz, 0));
    } else {
        operands.push(token);
    }
}

fn synth_push_dst_src_op(
    next_u8: &mut impl FnMut() -> u8,
    tokens: &mut Vec<u32>,
    opcode_raw: u32,
    specific: u8,
) {
    let predicated = (next_u8() & 3) == 0;
    let coissue = (next_u8() & 3) == 0;
    let src_count = 1 + (next_u8() % 3) as usize;

    let mut operands: Vec<u32> = Vec::new();
    let dst_reg = u16::from(next_u8());
    let write_mask = (next_u8() & 0xF).max(1);
    operands.push(encode_dst_token(0, dst_reg, write_mask));
    for _ in 0..src_count {
        synth_push_src_tokens(next_u8, &mut operands, true);
    }
    if predicated {
        operands.push(synth_predicate_token(next_u8));
    }

    let len = 1 + operands.len();
    debug_assert!(len <= 15);
    let mut opcode_token = synth_opcode_token(opcode_raw, len) | (u32::from(specific) << 16);
    if predicated {
        opcode_token |= PREDICATED_BIT;
    }
    if coissue {
        opcode_token |= COISSUE_BIT;
    }
    tokens.push(opcode_token);
    tokens.extend_from_slice(&operands);
}

fn synth_push_src_op(next_u8: &mut impl FnMut() -> u8, tokens: &mut Vec<u32>, opcode_raw: u32) {
    let predicated = (next_u8() & 3) == 0;
    let coissue = (next_u8() & 3) == 0;

    let src_count = 1 + (next_u8() % 3) as usize;
    let mut operands: Vec<u32> = Vec::new();
    for _ in 0..src_count {
        synth_push_src_tokens(next_u8, &mut operands, true);
    }
    if predicated {
        operands.push(synth_predicate_token(next_u8));
    }

    let len = 1 + operands.len();
    debug_assert!(len <= 15);
    let mut opcode_token = synth_opcode_token(opcode_raw, len);
    if predicated {
        opcode_token |= PREDICATED_BIT;
    }
    if coissue {
        opcode_token |= COISSUE_BIT;
    }
    tokens.push(opcode_token);
    tokens.extend_from_slice(&operands);
}

fn synth_push_control_op(
    next_u8: &mut impl FnMut() -> u8,
    tokens: &mut Vec<u32>,
    opcode_raw: u32,
) {
    // Control-flow ops (else/endif/ret) don't require operands, but allow optional predication so we
    // exercise that decode path.
    let predicated = (next_u8() & 7) == 0;
    let coissue = (next_u8() & 7) == 0;

    let mut operands: Vec<u32> = Vec::new();
    if predicated {
        operands.push(synth_predicate_token(next_u8));
    }

    let len = 1 + operands.len();
    debug_assert!(len <= 15);
    let mut opcode_token = synth_opcode_token(opcode_raw, len);
    if predicated {
        opcode_token |= PREDICATED_BIT;
    }
    if coissue {
        opcode_token |= COISSUE_BIT;
    }
    tokens.push(opcode_token);
    tokens.extend_from_slice(&operands);
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
        match next_u8() % 12 {
            // NOP: may still carry src operands (not semantically meaningful, but valid).
            0 => {
                let predicated = (next_u8() & 3) == 0;
                let coissue = (next_u8() & 3) == 0;
                let src_count = (next_u8() % 3) as usize;
                let mut operands: Vec<u32> = Vec::new();
                for _ in 0..src_count {
                    synth_push_src_tokens(&mut next_u8, &mut operands, true);
                }
                if predicated {
                    operands.push(synth_predicate_token(&mut next_u8));
                }
                let len = 1 + operands.len();
                debug_assert!(len <= 15);
                let mut opcode_token = synth_opcode_token(0, len);
                if predicated {
                    opcode_token |= PREDICATED_BIT;
                }
                if coissue {
                    opcode_token |= COISSUE_BIT;
                }
                tokens.push(opcode_token);
                tokens.extend_from_slice(&operands);
            }
            // dst + src ops
            1 => synth_push_dst_src_op(&mut next_u8, &mut tokens, 1, 0),   // mov
            2 => synth_push_dst_src_op(&mut next_u8, &mut tokens, 2, 0),   // add
            3 => synth_push_dst_src_op(&mut next_u8, &mut tokens, 4, 0),   // mad
            4 => synth_push_dst_src_op(&mut next_u8, &mut tokens, OPCODE_DP2ADD, 0),
            5 => synth_push_dst_src_op(&mut next_u8, &mut tokens, OPCODE_SETP, 0),
            6 => {
                // TEXLD / TEXLDP / TEXLDB are distinguished by `specific`.
                let specific = next_u8() % 3;
                synth_push_dst_src_op(&mut next_u8, &mut tokens, OPCODE_TEXLD, specific);
            }
            // src-only ops
            7 => synth_push_src_op(&mut next_u8, &mut tokens, OPCODE_IF),
            8 => synth_push_src_op(&mut next_u8, &mut tokens, OPCODE_TEXKILL),
            // control-flow-ish ops (not strictly validated, but include src operands for coverage)
            9 => synth_push_src_op(&mut next_u8, &mut tokens, OPCODE_BREAKC),
            // DCL (sampler or semantic declaration).
            10 => {
                let sampler = (next_u8() & 1) == 0;
                if sampler {
                    let texture_type = u32::from(next_u8() & 0xF);
                    let sampler_reg = u16::from(next_u8() % 16);
                    let opcode_token =
                        synth_opcode_token(OPCODE_DCL, 2) | (texture_type << 16);
                    tokens.push(opcode_token);
                    tokens.push(encode_dst_token(10, sampler_reg, 0xF));
                } else {
                    let usage_raw = if (next_u8() & 1) == 0 { 5u32 } else { 10u32 }; // texcoord/color
                    let usage_index = u32::from(next_u8() & 0xF);
                    let opcode_token = synth_opcode_token(OPCODE_DCL, 2)
                        | (usage_raw << 16)
                        | (usage_index << 20);
                    tokens.push(opcode_token);
                    let reg_ty = if is_pixel { 3u8 } else { 1u8 }; // t# (ps) or v# (vs)
                    let reg_num = u16::from(next_u8() % 16);
                    tokens.push(encode_dst_token(reg_ty, reg_num, 0xF));
                }
            }
            // COMMENT block (length in DWORDs lives in bits 16..30).
            11 => {
                let comment_len = u32::from(next_u8() % 4);
                tokens.push(OPCODE_COMMENT | (comment_len << 16));
                for _ in 0..comment_len {
                    let w = u32::from_le_bytes([next_u8(), next_u8(), next_u8(), next_u8()]);
                    tokens.push(w);
                }
            }
            // `next_u8() % 12` is mathematically bounded, but keep the match exhaustiveness checker
            // happy (and avoid relying on compiler range analysis).
            _ => synth_push_control_op(&mut next_u8, &mut tokens, OPCODE_RET),
        }

        // Keep the stream balanced-ish by occasionally inserting ELSE/ENDIF without constructing a
        // full CFG. The parser doesn't validate nesting, but the disassembler formats these ops.
        if (next_u8() & 0xF) == 0 {
            synth_push_control_op(&mut next_u8, &mut tokens, OPCODE_ELSE);
        }
        if (next_u8() & 0xF) == 0 {
            synth_push_control_op(&mut next_u8, &mut tokens, OPCODE_ENDIF);
        }
        if (next_u8() & 0xF) == 0 {
            synth_push_control_op(&mut next_u8, &mut tokens, OPCODE_RET);
        }
    }

    // END token.
    tokens.push(OPCODE_END);

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
