#![no_main]

use libfuzzer_sys::fuzz_target;

use aero_usb::hid::parse_report_descriptor;

// Keep descriptor sizes bounded so fuzz runs remain fast and deterministic.
const MAX_LEN: usize = 4096;

fuzz_target!(|data: &[u8]| {
    let bytes = if data.len() > MAX_LEN {
        &data[..MAX_LEN]
    } else {
        data
    };
    let _ = parse_report_descriptor(bytes);
});

