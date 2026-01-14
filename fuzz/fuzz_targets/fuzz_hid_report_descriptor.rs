#![no_main]

use libfuzzer_sys::fuzz_target;

use aero_usb::hid::{parse_report_descriptor, synthesize_report_descriptor, validate_collections};

// Keep descriptor sizes bounded so fuzz runs remain fast and deterministic.
const MAX_LEN: usize = 4096;

fuzz_target!(|data: &[u8]| {
    let bytes = if data.len() > MAX_LEN {
        &data[..MAX_LEN]
    } else {
        data
    };
    let Ok(collections) = parse_report_descriptor(bytes) else {
        return;
    };

    // Exercise the validator + synthesizer paths on successfully parsed descriptors. Both functions
    // are defensive and should never panic, even if validation fails.
    let _ = validate_collections(&collections);
    if let Ok(synth) = synthesize_report_descriptor(&collections) {
        let _ = parse_report_descriptor(&synth);
    }
});
