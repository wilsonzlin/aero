#![no_main]

use aero_d3d9::sm3::{build_ir, decode_u8_le_bytes, generate_wgsl, verify_ir};
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations in decode/IR/WGSL paths.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

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

fn opcode_token(op: u32, operand_tokens: u32) -> u32 {
    // D3D9 SM2/SM3 encodes the total instruction length in tokens (including the opcode token)
    // in bits 24..27.
    let length = operand_tokens.saturating_add(1);
    (op & 0xFFFF) | (length << 24)
}

fn build_patched_shader(seed: &[u8]) -> Vec<u8> {
    let stage_is_pixel = seed.get(0).copied().unwrap_or(0) & 1 != 0;
    let major = 2u32 + ((seed.get(1).copied().unwrap_or(0) as u32) & 1); // {2,3}
    let minor = 0u32;
    let high = if stage_is_pixel {
        0xFFFF_0000
    } else {
        0xFFFE_0000
    };
    let version_token = high | (major << 8) | minor;

    let op_sel = seed.get(2).copied().unwrap_or(0) % 3;
    let swz = 0xE4u8; // xyzw

    let (dst_regtype, dst_index) = if stage_is_pixel {
        (8u8, 0u8)
    } else {
        (4u8, 0u8)
    };
    let dst = dst_token(dst_regtype, dst_index, 0);

    let c0 = seed.get(3).copied().unwrap_or(0) % 8;
    let c1 = seed.get(4).copied().unwrap_or(1) % 8;
    let src0 = src_token(2, c0, swz, 0);
    let src1 = src_token(2, c1, swz, 0);

    let include_def = seed.get(5).copied().unwrap_or(0) & 1 != 0;
    let def_dst = dst_token(2, c0, 0);

    let mut tokens: Vec<u32> = Vec::with_capacity(16);
    tokens.push(version_token);
    if include_def {
        // def c#, imm0..imm3
        tokens.push(opcode_token(81, 5));
        tokens.push(def_dst);
        tokens.push(u32_from_seed(seed, 8));
        tokens.push(u32_from_seed(seed, 12));
        tokens.push(u32_from_seed(seed, 16));
        tokens.push(u32_from_seed(seed, 20));
    }

    match op_sel {
        0 => {
            // mov dst, src0
            tokens.push(opcode_token(1, 2));
            tokens.push(dst);
            tokens.push(src0);
        }
        1 => {
            // add dst, src0, src1
            tokens.push(opcode_token(2, 3));
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src1);
        }
        _ => {
            // cmp dst, cond=src0, src_ge=src0, src_lt=src1
            tokens.push(opcode_token(88, 4));
            tokens.push(dst);
            tokens.push(src0);
            tokens.push(src0);
            tokens.push(src1);
        }
    }

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

    // Fast reject: avoid doing a full tokenization when the first token cannot possibly be a
    // version token.
    if data.len() < 4 || (data.len() % 4) != 0 {
        decode_build_wgsl(data);
        return;
    }

    let first = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let first_high = first & 0xFFFF_0000;
    if (first_high == 0xFFFE_0000 || first_high == 0xFFFF_0000)
        && data.len() <= MAX_RAW_DECODE_BYTES
    {
        decode_build_wgsl(data);
    }

    // Forced-version variant (to reach deeper decode/IR/WGSL paths).
    let forced_len = data.len().min(MAX_FORCED_VERSION_BYTES) & !3;
    if forced_len < 4 {
        return;
    }

    let mut forced = data[..forced_len].to_vec();

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
