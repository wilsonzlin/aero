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

    let mode = seed.get(6).copied().unwrap_or(0) % 10;

    // For the texture-sampling path, force a pixel shader so we can exercise `texldd`.
    let stage_is_pixel = ((seed.get(0).copied().unwrap_or(0) & 1 != 0) || mode == 4) && mode != 7;

    // For the `mova`/relative-constant, `loop`, `predicate`, `dcl`, and `subroutine` modes, force
    // SM3 so address/loop/predicate/label registers are valid and we can exercise deeper structured
    // control-flow and subroutine lowering paths more reliably.
    let major = if mode == 2 || mode == 5 || mode == 6 || mode == 7 || mode == 9 {
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
    let c2 = seed.get(12).copied().unwrap_or(2) % 8;
    let src0 = src_token(2, c0, swz, src_mod);
    let src1 = src_token(2, c1, swz, src_mod);
    let src2 = src_token(2, c2, swz, src_mod);

    let include_def = seed.get(5).copied().unwrap_or(0) & 1 != 0;
    let def_dst = dst_token(2, c0, 0);

    let mut tokens: Vec<u32> = Vec::with_capacity(24);
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
            let use_ifc = seed.get(17).copied().unwrap_or(0) & 1 != 0;
            if use_ifc {
                // ifc src0, src1 (comparison op encoded in opcode_token[16..19])
                let cmp = (seed.get(18).copied().unwrap_or(0) % 6) as u32;
                tokens.push(opcode_token(41, 2, 0) | (cmp << 16));
                tokens.push(src0);
                tokens.push(src1);
            } else {
                // if src0
                tokens.push(opcode_token(40, 1, 0));
                tokens.push(src0);
            }
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
        3 => match seed.get(2).copied().unwrap_or(0) % 5 {
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
            2 => {
                tokens.push(opcode_token(32, 3, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
                tokens.push(src1);
            }
            // dp2add dst, src0, src1, src2
            3 => {
                tokens.push(opcode_token(89, 4, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
                tokens.push(src1);
                tokens.push(src2);
            }
            // lrp dst, src0, src1, src2
            _ => {
                tokens.push(opcode_token(18, 4, mod_bits));
                tokens.push(dst);
                tokens.push(src0);
                tokens.push(src1);
                tokens.push(src2);
            }
        },

        // Texture sampling (D3D9 `tex`/`texldl`/`texldd`).
        4 => {
            let sampler = seed.get(12).copied().unwrap_or(0) % 4;
            let sampler_token = src_token(10, sampler, 0xE4, 0);

            // Declare sampler type. Use a small subset of valid texture types; this helps WGSL
            // lowering reach texture-type-specific paths (including the 1D bias workaround).
            let sampler_ty = match seed.get(21).copied().unwrap_or(0) % 4 {
                0 => 1u32, // 1D
                1 => 2u32, // 2D
                2 => 3u32, // cube
                _ => 4u32, // 3D
            };
            tokens.push(opcode_token(31, 1, 0) | (sampler_ty << 16));
            tokens.push(dst_token(10, sampler, 0));

            // Optionally add predication around derivative ops / texture sampling so we exercise
            // WGSL's branchless predication lowering for uniform-control-flow-sensitive ops.
            let predicated = seed.get(13).copied().unwrap_or(0) & 1 != 0;
            let pred_is_prefix = seed.get(13).copied().unwrap_or(0) & 2 != 0;
            let pred_neg = (seed.get(13).copied().unwrap_or(0) >> 2) & 1;
            let pred_token = src_token(19, 0, 0x00, pred_neg);

            if predicated {
                // setp p0.x, src0, src1
                let p0 = dst_token(19, 0, 0x1);
                let cmp = (seed.get(14).copied().unwrap_or(0) % 6) as u32;
                tokens.push(opcode_token(78, 3, 0) | (cmp << 16));
                tokens.push(p0);
                tokens.push(src0);
                tokens.push(src1);
            }

            // Derivative ops (pixel shaders only): dsx/dsy.
            let r0 = dst_token(0, 0, 0);
            let r1 = dst_token(0, 1, 0);
            let emit_predicated_op2 = |tokens: &mut Vec<u32>, opcode: u32, dst: u32, src: u32| {
                if predicated {
                    let op = opcode_token(opcode, 3, mod_bits) | 0x1000_0000;
                    tokens.push(op);
                    if pred_is_prefix {
                        tokens.push(pred_token);
                        tokens.push(dst);
                        tokens.push(src);
                    } else {
                        tokens.push(dst);
                        tokens.push(src);
                        tokens.push(pred_token);
                    }
                } else {
                    tokens.push(opcode_token(opcode, 2, mod_bits));
                    tokens.push(dst);
                    tokens.push(src);
                }
            };
            emit_predicated_op2(&mut tokens, 86, r0, src0);
            emit_predicated_op2(&mut tokens, 87, r1, src0);

            match seed.get(2).copied().unwrap_or(0) % 4 {
                // texld / texldp (implicit LOD)
                0 => {
                    let specific = if seed.get(15).copied().unwrap_or(0) & 1 != 0 {
                        1u32 // texldp (project)
                    } else {
                        0u32 // texld
                    };
                    let op = if predicated {
                        opcode_token(66, 4, mod_bits) | 0x1000_0000 | (specific << 16)
                    } else {
                        opcode_token(66, 3, mod_bits) | (specific << 16)
                    };
                    tokens.push(op);
                    if predicated && pred_is_prefix {
                        tokens.push(pred_token);
                    }
                    tokens.push(dst);
                    tokens.push(src0);
                    tokens.push(sampler_token);
                    if predicated && !pred_is_prefix {
                        tokens.push(pred_token);
                    }
                }
                // texldb (bias) - pixel shaders only.
                1 => {
                    let specific = 2u32;
                    let op = if predicated {
                        opcode_token(66, 4, mod_bits) | 0x1000_0000 | (specific << 16)
                    } else {
                        opcode_token(66, 3, mod_bits) | (specific << 16)
                    };
                    tokens.push(op);
                    if predicated && pred_is_prefix {
                        tokens.push(pred_token);
                    }
                    tokens.push(dst);
                    tokens.push(src0);
                    tokens.push(sampler_token);
                    if predicated && !pred_is_prefix {
                        tokens.push(pred_token);
                    }
                }
                // texldl (explicit LOD)
                2 => {
                    let op = if predicated {
                        opcode_token(79, 4, mod_bits) | 0x1000_0000
                    } else {
                        opcode_token(79, 3, mod_bits)
                    };
                    tokens.push(op);
                    if predicated && pred_is_prefix {
                        tokens.push(pred_token);
                    }
                    tokens.push(dst);
                    tokens.push(src0);
                    tokens.push(sampler_token);
                    if predicated && !pred_is_prefix {
                        tokens.push(pred_token);
                    }
                }
                // texldd (gradients) - pixel shaders only.
                _ => {
                    let r0_src = src_token(0, 0, swz, src_mod);
                    let r1_src = src_token(0, 1, swz, src_mod);
                    let op = if predicated {
                        opcode_token(77, 6, mod_bits) | 0x1000_0000
                    } else {
                        opcode_token(77, 5, mod_bits)
                    };
                    tokens.push(op);
                    if predicated && pred_is_prefix {
                        tokens.push(pred_token);
                    }
                    tokens.push(dst);
                    tokens.push(src0);
                    tokens.push(r0_src);
                    tokens.push(r1_src);
                    tokens.push(sampler_token);
                    if predicated && !pred_is_prefix {
                        tokens.push(pred_token);
                    }
                }
            }

            // Optionally include a discard path (texkill) to exercise discard lowering.
            if seed.get(16).copied().unwrap_or(0) & 1 != 0 {
                tokens.push(opcode_token(65, 1, 0));
                tokens.push(src0);
            }
        }

        // Loop + breakc to exercise structured looping WGSL lowering.
        5 => {
            // loop aL#, i#
            let loop_reg_idx = seed.get(13).copied().unwrap_or(0) % 4;
            let ctrl_reg_idx = seed.get(14).copied().unwrap_or(0) % 4;
            let loop_reg = src_token(15, loop_reg_idx, 0xE4, 0);
            let ctrl_reg = src_token(7, ctrl_reg_idx, 0xE4, 0);
            tokens.push(opcode_token(27, 2, 0));
            tokens.push(loop_reg);
            tokens.push(ctrl_reg);

            // add dst, src0, src1
            tokens.push(opcode_token(2, 3, mod_bits));
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src1);

            // breakc src0, src1 (compare op encoded in opcode_token[16..19])
            let cmp = (seed.get(15).copied().unwrap_or(0) % 6) as u32;
            tokens.push(opcode_token(45, 2, 0) | (cmp << 16));
            tokens.push(src0);
            tokens.push(src1);

            // endloop
            tokens.push(opcode_token(29, 0, 0));
        }

        // Predication + setp to exercise predicate-aware WGSL lowering.
        6 => {
            // setp p0.x, src0, src1 (comparison op in opcode_token[16..19])
            let p0 = dst_token(19, 0, 0x1);
            let cmp = (seed.get(13).copied().unwrap_or(0) % 6) as u32;
            tokens.push(opcode_token(78, 3, 0) | (cmp << 16));
            tokens.push(p0);
            tokens.push(src0);
            tokens.push(src1);

            // Predicated add dst, src0, src1, p0.x (optionally negated).
            let pred_neg = seed.get(14).copied().unwrap_or(0) & 1;
            let pred_token = src_token(19, 0, 0x00, pred_neg);
            tokens.push(opcode_token(2, 4, mod_bits) | 0x1000_0000);
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src1);
            tokens.push(pred_token);
        }

        // DCL + vertex input semantic remapping to exercise input interface generation.
        7 => {
            let v_pos = seed.get(13).copied().unwrap_or(1) % 15 + 1;
            let mut v_norm = seed.get(14).copied().unwrap_or(2) % 15 + 1;
            while v_norm == v_pos {
                v_norm = (v_norm % 15) + 1;
            }
            let mut v_tex = seed.get(15).copied().unwrap_or(3) % 15 + 1;
            while v_tex == v_pos || v_tex == v_norm {
                v_tex = (v_tex % 15) + 1;
            }
            let tex_usage_index = seed.get(16).copied().unwrap_or(0) % 8;

            // dcl_position v#
            tokens.push(opcode_token(31, 1, 0) | (0u32 << 16));
            tokens.push(dst_token(1, v_pos, 0));
            // dcl_normal v#
            tokens.push(opcode_token(31, 1, 0) | (3u32 << 16));
            tokens.push(dst_token(1, v_norm, 0));
            // dcl_texcoord{tex_usage_index} v#
            tokens.push(opcode_token(31, 1, tex_usage_index) | (5u32 << 16));
            tokens.push(dst_token(1, v_tex, 0));

            // add dst, v_pos, v_tex
            let v_pos_src = src_token(1, v_pos, swz, src_mod);
            let v_tex_src = src_token(1, v_tex, swz, src_mod);
            tokens.push(opcode_token(2, 3, mod_bits));
            tokens.push(dst);
            tokens.push(v_pos_src);
            tokens.push(v_tex_src);
        }

        // Rep + endrep to exercise count-controlled loops.
        8 => {
            let i_idx = seed.get(17).copied().unwrap_or(0) % 4;
            let i_dst = dst_token(7, i_idx, 0);

            // defi i#, imm0..imm3
            tokens.push(opcode_token(82, 5, 0));
            tokens.push(i_dst);
            tokens.push(u32_from_seed(seed, 24));
            tokens.push(u32_from_seed(seed, 28));
            tokens.push(u32_from_seed(seed, 32));
            tokens.push(u32_from_seed(seed, 36));

            // rep i#
            let count_reg = src_token(7, i_idx, 0xE4, 0);
            tokens.push(opcode_token(38, 1, 0));
            tokens.push(count_reg);

            // add dst, src0, src1
            tokens.push(opcode_token(2, 3, mod_bits));
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src1);

            // break
            if seed.get(19).copied().unwrap_or(0) & 1 != 0 {
                tokens.push(opcode_token(44, 0, 0));
            }

            // endrep
            tokens.push(opcode_token(39, 0, 0));
        }

        // Call/label/ret subroutines (SM3).
        9 => {
            let label_idx = seed.get(13).copied().unwrap_or(0) % 4;
            let label_token = src_token(18, label_idx, 0xE4, 0);

            // call/callnz l#, cond
            let use_callnz = seed.get(14).copied().unwrap_or(0) & 1 != 0;
            if use_callnz {
                // callnz: label, cond
                tokens.push(opcode_token(26, 2, 0));
                tokens.push(label_token);
                tokens.push(src0);
            } else {
                // call: label
                tokens.push(opcode_token(25, 1, 0));
                tokens.push(label_token);
            }

            // ret (terminate main program before subroutine bodies)
            tokens.push(opcode_token(28, 0, 0));

            // label l#
            tokens.push(opcode_token(30, 1, 0));
            tokens.push(label_token);

            // subroutine body: add dst, src0, src1
            tokens.push(opcode_token(2, 3, mod_bits));
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src1);

            // ret
            tokens.push(opcode_token(28, 0, 0));
        }

        _ => unreachable!("mode is reduced modulo 10"),
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
