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
    encode_regtype(regtype)
        | (index as u32)
        | ((swizzle as u32) << 16)
        | ((modifier as u32) << 24)
}

fn u32_from_seed(seed: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        *seed.get(offset).unwrap_or(&0),
        *seed.get(offset + 1).unwrap_or(&0),
        *seed.get(offset + 2).unwrap_or(&0),
        *seed.get(offset + 3).unwrap_or(&0),
    ])
}

fn build_patched_shader(seed: &[u8]) -> Vec<u8> {
    // Build a tiny, self-consistent shader token stream derived from the fuzzer input.
    // This helps libFuzzer reach deeper decode/IR paths without having to discover the
    // version/opcode encodings from scratch.

    let stage_is_pixel = seed.get(0).copied().unwrap_or(0) & 1 != 0;
    let major = 2u32 + ((seed.get(1).copied().unwrap_or(0) as u32) & 1); // {2,3}
    let minor = 0u32;
    let high = if stage_is_pixel {
        0xFFFF_0000
    } else {
        0xFFFE_0000
    };
    let version_token = high | (major << 8) | minor;

    // Pick a simple opcode that is supported by the IR builder.
    let op_sel = seed.get(2).copied().unwrap_or(0) % 3;
    let swz = 0xE4u8; // xyzw

    // Destination: write to the "primary output" register for the chosen stage.
    // - VS: oPos (RastOut type=4, index 0)
    // - PS: oC0 (ColorOut type=8, index 0)
    let (dst_regtype, dst_index) = if stage_is_pixel { (8u8, 0u8) } else { (4u8, 0u8) };
    let dst = dst_token(dst_regtype, dst_index, 0);

    // Sources: float constants c0/c1 (type=2) with varying indices.
    let c0 = seed.get(3).copied().unwrap_or(0) % 8;
    let c1 = seed.get(4).copied().unwrap_or(1) % 8;
    let src0 = src_token(2, c0, swz, 0);
    let src1 = src_token(2, c1, swz, 0);

    // Optional embedded constant definition for c0.
    let include_def = seed.get(5).copied().unwrap_or(0) & 1 != 0;
    let def_dst = dst_token(2, c0, 0);

    let mut tokens: Vec<u32> = Vec::with_capacity(16);
    tokens.push(version_token);
    if include_def {
        // def c#, imm0..imm3
        tokens.push((5u32 << 24) | 81u32);
        tokens.push(def_dst);
        tokens.push(u32_from_seed(seed, 8));
        tokens.push(u32_from_seed(seed, 12));
        tokens.push(u32_from_seed(seed, 16));
        tokens.push(u32_from_seed(seed, 20));
    }

    match op_sel {
        // mov dst, src0
        0 => {
            tokens.push((2u32 << 24) | 1u32);
            tokens.push(dst);
            tokens.push(src0);
        }
        // add dst, src0, src1
        1 => {
            tokens.push((3u32 << 24) | 2u32);
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src1);
        }
        // cmp dst, src0, src1, src0 (cond=src0)
        _ => {
            tokens.push((4u32 << 24) | 88u32);
            tokens.push(dst);
            tokens.push(src0); // cond
            tokens.push(src0); // src_ge
            tokens.push(src1); // src_lt
        }
    }

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

    // Pre-filter absurd `chunk_count` values to avoid worst-case O(n) DXBC parsing time and
    // unbounded intermediate allocations.
    let mut allow_dxbc = true;
    if data.len() >= 32 && &data[..4] == b"DXBC" {
        let chunk_count = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        if chunk_count > MAX_DXBC_CHUNKS {
            allow_dxbc = false;
        }
    }

    // Exercise DXBC container parsing (best-effort).
    if allow_dxbc {
        let _ = aero_d3d9::dxbc::robust::DxbcContainer::parse(data);
    }

    // Exercise the legacy D3D9 shader parser (DXBC/raw SM2/3 token stream → ShaderProgram).
    //
    // This parser tokenizes the entire input into u32 tokens up front, so avoid calling it on
    // large buffers unless the first token looks like a plausible version token.
    if data.len() <= MAX_RAW_DECODE_BYTES {
        if allow_dxbc && data.len() >= 4 && &data[..4] == b"DXBC" {
            let _ = aero_d3d9::shader::parse(data);
        } else if data.len() < 4 || (data.len() % 4) != 0 {
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
        let first = u32::from_le_bytes([token_bytes[0], token_bytes[1], token_bytes[2], token_bytes[3]]);
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

    let high = if stage_is_pixel { 0xFFFF_0000 } else { 0xFFFE_0000 };
    let version_token = high | (major << 8) | minor;
    forced[..4].copy_from_slice(&version_token.to_le_bytes());

    // Also run the legacy parser on the forced variant to reach deeper parsing paths without
    // requiring libFuzzer to guess the magic version bits.
    let _ = aero_d3d9::shader::parse(&forced);

    decode_build_verify(&forced);
});
