#![no_main]

use aero_d3d9::sm3::{build_ir, decode_u8_le_bytes, verify_ir};
use libfuzzer_sys::fuzz_target;

/// Max fuzz input size to avoid pathological allocations in the token decoder.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // Treat the input as untrusted shader bytecode (SM2/3 token stream).
    // All decode/build failures are acceptable; the oracle is "must not panic".
    let Ok(decoded) = decode_u8_le_bytes(data) else {
        return;
    };

    if let Ok(ir) = build_ir(&decoded) {
        let _ = verify_ir(&ir);
    }
});

