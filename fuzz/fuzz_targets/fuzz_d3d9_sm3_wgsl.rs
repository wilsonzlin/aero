#![no_main]

use aero_d3d9::sm3::{build_ir, decode_u8_le_bytes, generate_wgsl, verify_ir};
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations in decode/IR/WGSL paths.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// `aero_d3d9::dxbc` parsing validates each chunk offset in a loop. Cap `chunk_count` for
/// deterministic fuzz iteration cost when the input happens to look like a DXBC container.
const MAX_DXBC_CHUNKS: u32 = 1024;

/// Even with an overall input cap, decoding and WGSL generation can become very slow for
/// long token streams (large instruction counts and large generated strings). Keep the
/// "decode the full input" path smaller, and rely on the forced-version (truncated) path
/// for coverage on large inputs.
const MAX_RAW_DECODE_BYTES: usize = 256 * 1024; // 256 KiB

/// When the input doesn't contain a valid SM2/3 version token, also try a "forced version"
/// variant to reach deeper decode/IR/WGSL paths without requiring libFuzzer to guess the magic
/// bits.
///
/// Keep this small to avoid copying 1MiB inputs every iteration.
const MAX_FORCED_VERSION_BYTES: usize = 64 * 1024; // 64 KiB

/// Cap decoded instruction count before building IR / generating WGSL to avoid pathological
/// allocations/time in those stages.
const MAX_INSTRUCTIONS: usize = 4096;

fn encode_regtype(raw: u8) -> u32 {
    let low = (raw & 0x7) as u32;
    let high = (raw & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn dst_token(regtype: u8, index: u8, mask: u8) -> u32 {
    let mut t = encode_regtype(regtype) | (index as u32);
    t |= ((mask & 0xF) as u32) << 16;
    t
}

fn src_token(regtype: u8, index: u8, swizzle: u8, modifier: u8) -> u32 {
    encode_regtype(regtype) | (index as u32) | ((swizzle as u32) << 16) | ((modifier as u32) << 24)
}

fn u32_from_seed(seed: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        *seed.get(offset).unwrap_or(&0),
        *seed.get(offset + 1).unwrap_or(&0),
        *seed.get(offset + 2).unwrap_or(&0),
        *seed.get(offset + 3).unwrap_or(&0),
    ])
}

fn opcode_token(op: u32, operand_tokens: u32, mod_bits: u8) -> u32 {
    // D3D9 SM2/SM3 encodes the total instruction length in tokens (including the opcode token)
    // in bits 24..27.
    let length = operand_tokens.saturating_add(1);
    (op & 0xFFFF) | (length << 24) | ((mod_bits as u32) << 20)
}

fn src_token_rel_const(
    base_idx: u8,
    swizzle: u8,
    modifier: u8,
    rel_reg_idx: u8,
    rel_swizzle: u8,
) -> [u32; 2] {
    // Relative constant addressing: set RELATIVE bit on the base constant token and append a
    // second token describing the relative register (address register for `cN[a0.x]`).
    const RELATIVE: u32 = 0x0000_2000;
    let base = encode_regtype(2)
        | (base_idx as u32)
        | RELATIVE
        | ((swizzle as u32) << 16)
        | ((modifier as u32) << 24);
    let rel = src_token(3, rel_reg_idx, rel_swizzle, 0);
    [base, rel]
}

fn build_patched_shader(seed: &[u8]) -> Vec<u8> {
    // Build a tiny, self-consistent shader token stream derived from the fuzzer input.
    // This helps libFuzzer reach deep decode/IR/WGSL paths without having to discover the
    // version/opcode encodings from scratch.

    let mode = seed.get(6).copied().unwrap_or(0) % 5;

    // For the texture-sampling path, force a pixel shader so we can exercise `texldd`.
    let stage_is_pixel = (seed.get(0).copied().unwrap_or(0) & 1 != 0) || mode == 4;

    // For the `mova`/relative-constant mode, force SM3 so address registers are valid.
    let major = if mode == 2 {
        3u32
    } else {
        2u32 + ((seed.get(1).copied().unwrap_or(0) as u32) & 1)
    };
    let minor = 0u32;
    let high = if stage_is_pixel {
        0xFFFF_0000
    } else {
        0xFFFE_0000
    };
    let version_token = high | (major << 8) | minor;

    // Result modifiers (saturate + shift) are encoded in opcode_token[20..24].
    let saturate = seed.get(7).copied().unwrap_or(0) & 1;
    let shift = seed.get(8).copied().unwrap_or(0) % 7; // 0..=6 (avoid Unknown)
    let mod_bits = (saturate & 1) | ((shift & 0x7) << 1);

    // Use a fuzzer-controlled swizzle/modifier, but keep modifier in the known range so we reach
    // deeper IR/WGSL paths (unknown modifiers are rejected by the IR builder).
    let swz = seed.get(9).copied().unwrap_or(0xE4); // xyzw default
    let src_mod = seed.get(10).copied().unwrap_or(0) % 14; // 0..=13

    let (dst_regtype, dst_index) = if stage_is_pixel {
        (8u8, 0u8)
    } else {
        (4u8, 0u8)
    };
    let dst_mask = seed.get(11).copied().unwrap_or(0) & 0xF;
    let dst = dst_token(dst_regtype, dst_index, dst_mask);

    let c0 = seed.get(3).copied().unwrap_or(0) % 8;
    let c1 = seed.get(4).copied().unwrap_or(1) % 8;
    let src0 = src_token(2, c0, swz, src_mod);
    let src1 = src_token(2, c1, swz, src_mod);

    let include_def = seed.get(5).copied().unwrap_or(0) & 1 != 0;
    let def_dst = dst_token(2, c0, 0);

    let mut tokens: Vec<u32> = Vec::with_capacity(16);
    tokens.push(version_token);
    if include_def {
        // def c#, imm0..imm3
        tokens.push(opcode_token(81, 5, 0));
        tokens.push(def_dst);
        tokens.push(u32_from_seed(seed, 8));
        tokens.push(u32_from_seed(seed, 12));
        tokens.push(u32_from_seed(seed, 16));
        tokens.push(u32_from_seed(seed, 20));
    }

    match mode {
        // Simple straight-line op.
        0 => match seed.get(2).copied().unwrap_or(0) % 3 {
            // mov dst, src0
            0 => {
                tokens.push(opcode_token(1, 2, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
            }
            // add dst, src0, src1
            1 => {
                tokens.push(opcode_token(2, 3, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
                tokens.push(src1);
            }
            // cmp dst, cond=src0, src_ge=src0, src_lt=src1
            _ => {
                tokens.push(opcode_token(88, 4, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
                tokens.push(src0);
                tokens.push(src1);
            }
        },

        // Simple control-flow: if/else/endif.
        1 => {
            // if src0
            tokens.push(opcode_token(40, 1, 0));
            tokens.push(src0);
            // then: mov
            tokens.push(opcode_token(1, 2, mod_bits));
            tokens.push(dst);
            tokens.push(src0);
            // else
            tokens.push(opcode_token(42, 0, 0));
            // else: add
            tokens.push(opcode_token(2, 3, mod_bits));
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src1);
            // endif
            tokens.push(opcode_token(43, 0, 0));
        }

        // Exercise address registers + relative constant addressing (SM3).
        2 => {
            // mova a0, src0
            let a0 = dst_token(3, 0, 0);
            tokens.push(opcode_token(46, 2, mod_bits));
            tokens.push(a0);
            tokens.push(src0);

            // mov dst, c1[a0.x]
            let rel = src_token_rel_const(c1, swz, src_mod, 0, 0xE4);
            tokens.push(opcode_token(1, 3, mod_bits));
            tokens.push(dst);
            tokens.push(rel[0]);
            tokens.push(rel[1]);
        }

        // A couple of math ops to reach more lowering code.
        3 => match seed.get(2).copied().unwrap_or(0) % 3 {
            // dp2 dst, src0, src1
            0 => {
                tokens.push(opcode_token(90, 3, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
                tokens.push(src1);
            }
            // exp dst, src0
            1 => {
                tokens.push(opcode_token(14, 2, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
            }
            // pow dst, src0, src1
            _ => {
                tokens.push(opcode_token(32, 3, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
                tokens.push(src1);
            }
        },

        // Texture sampling (D3D9 `tex`/`texldl`/`texldd`).
        _ => {
            let sampler = seed.get(12).copied().unwrap_or(0) % 4;
            let sampler_token = src_token(10, sampler, 0xE4, 0);

            // dcl_2d s#
            tokens.push(opcode_token(31, 1, 0) | (2u32 << 16));
            tokens.push(dst_token(10, sampler, 0));

            match seed.get(2).copied().unwrap_or(0) % 3 {
                // texld / texldp (implicit LOD)
                0 => {
                    let project = if seed.get(13).copied().unwrap_or(0) & 1 != 0 {
                        1u32 << 16
                    } else {
                        0
                    };
                    tokens.push(opcode_token(66, 3, mod_bits) | project);
                    tokens.push(dst);
                    tokens.push(src0);
                    tokens.push(sampler_token);
                }
                // texldl (explicit LOD)
                1 => {
                    tokens.push(opcode_token(79, 3, mod_bits));
                    tokens.push(dst);
                    tokens.push(src0);
                    tokens.push(sampler_token);
                }
                // texldd (gradients) - pixel shaders only.
                _ => {
                    tokens.push(opcode_token(77, 5, mod_bits));
                    tokens.push(dst);
                    tokens.push(src0);
                    tokens.push(src0);
                    tokens.push(src1);
                    tokens.push(sampler_token);
                }
            }
        }
    };

    tokens.push(0x0000_FFFF);

    let mut out = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn decode_build_wgsl(bytes: &[u8]) {
    let Ok(decoded) = decode_u8_le_bytes(bytes) else {
        return;
    };
    if decoded.instructions.len() > MAX_INSTRUCTIONS {
        return;
    }
    let Ok(ir) = build_ir(&decoded) else {
        return;
    };

    // Verify IR invariants (best-effort).
    let _ = verify_ir(&ir);

    // Exercise WGSL generation. Errors are acceptable; the oracle is "must not panic".
    if let Ok(out) = generate_wgsl(&ir) {
        let _ = (out.entry_point, out.wgsl.len());
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Always exercise a tiny, self-consistent shader derived from the input to reach deep
    // decode/IR/WGSL paths quickly.
    let patched = build_patched_shader(data);
    decode_build_wgsl(&patched);

    // Pre-filter absurd `chunk_count` values to avoid worst-case O(n) DXBC parsing time.
    let mut allow_dxbc = true;
    if data.len() >= 32 && &data[..4] == b"DXBC" {
        let chunk_count = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        if chunk_count > MAX_DXBC_CHUNKS {
            allow_dxbc = false;
        }
    }

    // Extract candidate shader bytecode if the input looks like a DXBC container; otherwise use
    // the input directly (the D3D9 runtime commonly provides raw DWORD token streams).
    let token_bytes = if allow_dxbc {
        match aero_d3d9::dxbc::extract_shader_bytecode(data) {
            Ok(bytes) => bytes,
            Err(_) => data,
        }
    } else {
        data
    };

    // Fast reject: avoid doing a full tokenization when the first token cannot possibly be a
    // version token.
    if token_bytes.len() < 4 || (token_bytes.len() % 4) != 0 {
        decode_build_wgsl(token_bytes);
        return;
    }

    let first = u32::from_le_bytes([
        token_bytes[0],
        token_bytes[1],
        token_bytes[2],
        token_bytes[3],
    ]);
    let first_high = first & 0xFFFF_0000;
    if (first_high == 0xFFFE_0000 || first_high == 0xFFFF_0000)
        && token_bytes.len() <= MAX_RAW_DECODE_BYTES
    {
        decode_build_wgsl(token_bytes);
    }

    // Forced-version variant (to reach deeper decode/IR/WGSL paths).
    let forced_len = token_bytes.len().min(MAX_FORCED_VERSION_BYTES) & !3;
    if forced_len < 4 {
        return;
    }

    let mut forced = token_bytes[..forced_len].to_vec();

    // D3D9 shader version token layout:
    //   high 16 bits: 0xFFFE (vs) or 0xFFFF (ps)
    //   low 16 bits:  major in bits 8..15, minor in bits 0..7
    let stage_is_pixel = (forced[0] & 1) != 0;
    let major = 2u32 + ((forced[1] as u32) & 1); // {2, 3}
    let minor = 0u32;

    let high = if stage_is_pixel {
        0xFFFF_0000
    } else {
        0xFFFE_0000
    };
    let version_token = high | (major << 8) | minor;
    forced[..4].copy_from_slice(&version_token.to_le_bytes());

    decode_build_wgsl(&forced);
});
