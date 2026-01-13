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

    // Fast reject: avoid doing a full tokenization when the first token cannot possibly be a
    // version token.
    if data.len() < 4 || (data.len() % 4) != 0 {
        decode_build_wgsl(data);
        return;
    }

    let first = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let first_high = first & 0xFFFF_0000;
    if (first_high == 0xFFFE_0000 || first_high == 0xFFFF_0000) && data.len() <= MAX_RAW_DECODE_BYTES {
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
