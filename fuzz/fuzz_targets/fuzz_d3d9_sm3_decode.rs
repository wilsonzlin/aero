#![no_main]

use aero_d3d9::sm3::{build_ir, decode_u8_le_bytes, verify_ir};
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations in decode/IR paths.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// `aero_d3d9::dxbc` parsing validates each chunk offset in a loop. Cap `chunk_count` for
/// deterministic fuzz iteration cost when the input happens to look like a DXBC container.
const MAX_DXBC_CHUNKS: u32 = 1024;

/// Even with an overall input cap, fully tokenizing large buffers is expensive. Keep the
/// "decode the full input" path smaller, and rely on the forced-version (truncated) path for
/// coverage on large inputs.
const MAX_RAW_DECODE_BYTES: usize = 256 * 1024; // 256 KiB

/// When the input doesn't contain a valid SM2/3 version token, also try a "forced version"
/// variant to reach deeper decode/IR paths without requiring libFuzzer to guess the magic bits.
///
/// Keep this small to avoid copying 1MiB inputs every iteration.
const MAX_FORCED_VERSION_BYTES: usize = 64 * 1024; // 64 KiB

/// Cap decoded instruction count before IR build/verification to avoid pathological allocations
/// and runtimes in later passes.
const MAX_INSTRUCTIONS: usize = 4096;

fn encode_regtype(raw: u8) -> u32 {
    // D3D9 register type encoding is split across:
    // - bits 28..30 (low 3 bits of the type)
    // - bits 11..12 (high 2 bits of the type)
    //
    // See `decode_register_ref` in `aero_d3d9::sm3::decode`.
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

fn align4(value: usize) -> usize {
    (value + 3) & !3
}

fn build_dxbc_container(chunks: &[([u8; 4], &[u8])]) -> Vec<u8> {
    // Minimal DXBC container builder (copied from `aero-dxbc` test utils).
    //
    // Layout:
    // - magic:      4 bytes ("DXBC")
    // - checksum:  16 bytes (unused here; all zeros)
    // - reserved:   4 bytes (typically 1)
    // - total_size: 4 bytes
    // - chunk_count:4 bytes
    // - chunk_offsets: chunk_count * 4 bytes
    // - chunks:
    //     - fourcc: 4 bytes
    //     - size:   4 bytes
    //     - data:   size bytes (padded to 4-byte alignment)
    let header_size = 4 + 16 + 4 + 4 + 4 + (4 * chunks.len());
    let chunk_bytes = chunks
        .iter()
        .map(|(_, data)| align4(8 + data.len()))
        .sum::<usize>();

    let mut out = Vec::with_capacity(header_size + chunk_bytes);

    out.extend_from_slice(b"DXBC");
    out.extend_from_slice(&[0u8; 16]); // checksum (MD5; ignored by parsers)
    out.extend_from_slice(&1u32.to_le_bytes()); // reserved
    out.extend_from_slice(&0u32.to_le_bytes()); // total_size placeholder
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());

    // Reserve space for the chunk offset table and fill it in once we know the offsets.
    let offsets_pos = out.len();
    out.resize(out.len() + 4 * chunks.len(), 0);

    let mut offsets = Vec::with_capacity(chunks.len());
    for (fourcc, data) in chunks {
        offsets.push(out.len() as u32);

        out.extend_from_slice(fourcc);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out.resize(align4(out.len()), 0);
    }

    // Fill offsets.
    for (i, offset) in offsets.iter().enumerate() {
        let pos = offsets_pos + i * 4;
        out[pos..pos + 4].copy_from_slice(&offset.to_le_bytes());
    }

    // Fill total_size.
    let total_size = out.len() as u32;
    let total_size_pos = 4 + 16 + 4;
    out[total_size_pos..total_size_pos + 4].copy_from_slice(&total_size.to_le_bytes());

    out
}

fn build_patched_shader(seed: &[u8]) -> Vec<u8> {
    // Build a tiny, self-consistent shader token stream derived from the fuzzer input.
    // This helps libFuzzer reach deeper decode/IR paths without having to discover the
    // version/opcode encodings from scratch.

    let mode = seed.get(6).copied().unwrap_or(0) % 9;
    let stage_is_pixel = (seed.get(0).copied().unwrap_or(0) & 1 != 0) && mode != 2 && mode != 6;
    // For the `mova`/relative-constant, `loop`, `predicate`, `dcl`, and `subroutine` modes, force
    // SM3 so address/loop/predicate/label registers are valid and we reach deeper semantic-remapping
    // and structured-control-flow IR paths more reliably.
    let major = if mode == 2 || mode == 4 || mode == 5 || mode == 6 || mode == 8 {
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

    // Destination: write to the "primary output" register for the chosen stage.
    // - VS: oPos (RastOut type=4, index 0)
    // - PS: oC0 (ColorOut type=8, index 0)
    let (dst_regtype, dst_index) = if stage_is_pixel {
        (8u8, 0u8)
    } else {
        (4u8, 0u8)
    };
    let dst_mask = seed.get(11).copied().unwrap_or(0) & 0xF;
    let dst = dst_token(dst_regtype, dst_index, dst_mask);

    // Sources: float constants c0/c1/c2 (type=2) with varying indices.
    let c0 = seed.get(3).copied().unwrap_or(0) % 8;
    let c1 = seed.get(4).copied().unwrap_or(1) % 8;
    let c2 = seed.get(12).copied().unwrap_or(2) % 8;
    let src0 = src_token(2, c0, swz, src_mod);
    let src1 = src_token(2, c1, swz, src_mod);
    let src2 = src_token(2, c2, swz, src_mod);

    // Optional embedded constant definition for c0.
    let include_def = seed.get(5).copied().unwrap_or(0) & 1 != 0;
    let def_dst = dst_token(2, c0, 0);

    let mut tokens: Vec<u32> = Vec::with_capacity(48);
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

        // Simple control-flow: if/else/endif around a couple of ops.
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

        // Exercise address registers + relative constant addressing (vs_3_0).
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

        // Loop + breakc to exercise structured looping IR.
        4 => {
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

        // Predication + setp to exercise predicate decoding and IR modifiers.
        5 => {
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

        // DCL + vertex input semantic remapping to exercise `apply_vertex_input_remap`.
        6 => {
            // Declare a couple of input registers with distinct semantics, then use them in a
            // simple op. Pick non-zero `v#` indices so remapping is likely to actually change the
            // register indices in the IR.
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
        7 => {
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
        8 => {
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

        _ => unreachable!("mode is reduced modulo 9"),
    };

    // End token.
    tokens.push(0x0000_FFFF);

    let mut out = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn decode_build_verify(bytes: &[u8]) {
    let Ok(decoded) = decode_u8_le_bytes(bytes) else {
        return;
    };
    if decoded.instructions.len() > MAX_INSTRUCTIONS {
        return;
    }
    if let Ok(ir) = build_ir(&decoded) {
        let _ = verify_ir(&ir);
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Always run a tiny, self-consistent shader derived from the input to hit deeper decode/IR
    // paths quickly (while still treating the raw input as hostile below).
    let patched = build_patched_shader(data);
    decode_build_verify(&patched);
    // Exercise the non-DXBC code path in `aero_d3d9::shader::parse`.
    let _ = aero_d3d9::shader::parse(&patched);
    // Also wrap the patched SM2/3 token stream in a minimal DXBC container to exercise the DXBC
    // parsing and shader-bytecode extraction entrypoints in `aero_d3d9::dxbc`.
    let patched_dxbc = build_dxbc_container(&[(*b"SHDR", patched.as_slice())]);
    let _ = aero_d3d9::shader::parse(&patched_dxbc);
    if let Ok(bytes) = aero_d3d9::dxbc::extract_shader_bytecode(&patched_dxbc) {
        decode_build_verify(bytes);
    }

    // Pre-filter absurd `chunk_count` values to avoid worst-case O(n) DXBC parsing time and
    // unbounded intermediate allocations.
    let mut allow_dxbc = true;
    if data.len() >= 32 && &data[..4] == b"DXBC" {
        let chunk_count = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        if chunk_count > MAX_DXBC_CHUNKS {
            allow_dxbc = false;
        }
    }

    // DXBC parsing is exercised via `dxbc::extract_shader_bytecode` / `shader::parse` below. Keep
    // this target in pure parsing/IR code without requiring optional DXBC parsing features.
    // Exercise the legacy D3D9 shader parser (DXBC/raw SM2/3 token stream → ShaderProgram).
    //
    // This parser tokenizes the entire input into u32 tokens up front, so avoid calling it on
    // large buffers unless the first token looks like a plausible version token.
    if allow_dxbc && data.len() >= 4 && &data[..4] == b"DXBC" {
        // `shader::parse` tokenizes only the extracted shader chunk (not the whole DXBC container),
        // so it is fine to run this even when the container itself is larger than
        // `MAX_RAW_DECODE_BYTES`.
        let _ = aero_d3d9::shader::parse(data);
    } else if data.len() <= MAX_RAW_DECODE_BYTES {
        if data.len() < 4 || (data.len() % 4) != 0 {
            // Cheap early-error path (no full tokenization).
            let _ = aero_d3d9::shader::parse(data);
        } else {
            let first = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let first_high = first & 0xFFFF_0000;
            if first_high == 0xFFFE_0000 || first_high == 0xFFFF_0000 {
                let _ = aero_d3d9::shader::parse(data);
            }
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

    // Treat the input as untrusted shader bytecode (SM2/3 token stream).
    // All decode/build failures are acceptable; the oracle is "must not panic".
    //
    // Fast reject: avoid doing a full tokenization when the first token cannot possibly be a
    // version token.
    if token_bytes.len() < 4 || (token_bytes.len() % 4) != 0 {
        decode_build_verify(token_bytes);
    } else {
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
            decode_build_verify(token_bytes);
        }
    }

    // Forced-version variant (to reach deeper decode/IR paths).
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

    // Also run the legacy parser on the forced variant to reach deeper parsing paths without
    // requiring libFuzzer to guess the magic version bits.
    let _ = aero_d3d9::shader::parse(&forced);

    decode_build_verify(&forced);
});
