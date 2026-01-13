#![no_main]

use libfuzzer_sys::fuzz_target;

use aero_l2_protocol::{decode_message, decode_with_limits, encode_with_limits, Limits};

const MAX_INPUT_LEN: usize = 4096;

fuzz_target!(|data: &[u8]| {
    // Keep the harness deterministic and avoid stressing allocator behavior with huge inputs.
    let data = &data[..data.len().min(MAX_INPUT_LEN)];

    // Exercise the default entrypoint (default limits).
    let _ = decode_message(data);

    // Also exercise the configurable decode path with broader limits so we can reach the encode
    // roundtrip even when the default control payload cap (256 bytes) would reject the message.
    let limits = Limits {
        max_frame_payload: MAX_INPUT_LEN,
        max_control_payload: MAX_INPUT_LEN,
    };

    if let Ok(msg) = decode_with_limits(data, &limits) {
        // If decode succeeds, re-encoding must not panic.
        let _ = encode_with_limits(msg.msg_type, msg.flags, msg.payload, &limits);
    }
});

