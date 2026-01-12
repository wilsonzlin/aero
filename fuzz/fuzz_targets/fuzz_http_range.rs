#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_http_range::{parse_range_header, resolve_ranges, MAX_RANGE_HEADER_LEN};

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Resource length for `resolve_ranges`.
    let len: u64 = u.arbitrary().unwrap_or(0);
    let coalesce: bool = u.arbitrary().unwrap_or(false);

    // Remaining bytes are treated as the Range header value. If the fuzzer input is short,
    // optionally expand it to a size near the header cap to stress boundary checks.
    let expand: bool = u.arbitrary().unwrap_or(false);
    let rest_len = u.len();
    let rest = u.bytes(rest_len).unwrap_or(&[]);

    let mut header_bytes = rest.to_vec();
    if expand && !header_bytes.is_empty() {
        let target_len = MAX_RANGE_HEADER_LEN.saturating_add(256);
        while header_bytes.len() < target_len {
            // Repeat the attacker-controlled bytes until we hit the desired size.
            let need = target_len - header_bytes.len();
            let take = need.min(rest.len());
            header_bytes.extend_from_slice(&rest[..take]);
            if take == 0 {
                break;
            }
        }
        header_bytes.truncate(target_len);
    }

    let header = String::from_utf8_lossy(&header_bytes);
    if let Ok(specs) = parse_range_header(&header) {
        let _ = resolve_ranges(&specs, len, coalesce);
    } else {
        // Still exercise parsing for error paths; the oracle is "must not panic".
        let _ = parse_range_header(&header);
    }
});

