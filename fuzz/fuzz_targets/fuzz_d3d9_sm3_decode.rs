#![no_main]

use aero_d3d9::sm3::{build_ir, decode_u8_le_bytes, verify_ir};
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations in the token decoder.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// When the input doesn't contain a valid SM2/3 version token, we also try a
/// "forced version" variant to reach deeper decode/IR paths without requiring
/// libFuzzer to guess the magic bits.
///
/// Keep this small to avoid copying 1MiB inputs every iteration.
const MAX_FORCED_VERSION_BYTES: usize = 64 * 1024; // 64 KiB

fn decode_build_verify(bytes: &[u8]) {
    let Ok(decoded) = decode_u8_le_bytes(bytes) else {
        return;
    };
    if let Ok(ir) = build_ir(&decoded) {
        let _ = verify_ir(&ir);
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Treat the input as untrusted shader bytecode (SM2/3 token stream).
    // All decode/build failures are acceptable; the oracle is "must not panic".
    //
    // `decode_u8_le_bytes` must convert the entire buffer into u32 tokens before
    // it can reject an invalid version token. Avoid that O(n) work for clearly
    // non-shader data by only attempting a full decode when the stage bits look
    // plausible (or when the length isn't even a valid token stream).
    if data.len() < 4 || (data.len() % 4) != 0 {
        decode_build_verify(data);
        return;
    }

    let first = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let first_high = first & 0xFFFF_0000;
    if first_high == 0xFFFE_0000 || first_high == 0xFFFF_0000 {
        decode_build_verify(data);
    }

    // If the input is large, the odds are high that it doesn't start with a
    // valid version token; avoid copying huge buffers just to patch 4 bytes.
    let forced_len = data.len().min(MAX_FORCED_VERSION_BYTES) & !3;
    if forced_len < 4 {
        return;
    }

    let mut forced = data[..forced_len].to_vec();

    // D3D9 shader version token layout:
    //   high 16 bits: 0xFFFE (vs) or 0xFFFF (ps)
    //   low 16 bits:  major in bits 8..15, minor in bits 0..7
    //
    // Choose stage/model from the input so libFuzzer can still influence it.
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

    decode_build_verify(&forced);
});
