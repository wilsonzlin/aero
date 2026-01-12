#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_http_range::{parse_range_header, resolve_ranges, MAX_RANGE_HEADER_LEN};

fn choose_target_len(seed: i16, base: usize, max: usize) -> usize {
    // Choose a length in [base-512, base+512], clamped into [0, max].
    let delta = (seed as i32) % 1025 - 512;
    let target = base as i32 + delta;
    target.clamp(0, max as i32) as usize
}

fn gen_decimal(u: &mut Unstructured<'_>, max_len: usize) -> String {
    let len: usize = u.int_in_range(0usize..=max_len).unwrap_or(0);
    if len == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        let d: u8 = u.arbitrary().unwrap_or(0);
        out.push(char::from(b'0' + (d % 10)));
    }
    out
}

fn gen_range_spec(u: &mut Unstructured<'_>) -> String {
    // Generate a mix of syntactically valid and invalid specs.
    match u.arbitrary::<u8>().unwrap_or(0) % 6 {
        // start-end
        0 => format!("{}-{}", gen_decimal(u, 64), gen_decimal(u, 64)),
        // start-
        1 => format!("{}-", gen_decimal(u, 64)),
        // -suffix
        2 => format!("-{}", gen_decimal(u, 64)),
        // garbage
        3 => gen_decimal(u, 64),
        // multiple '-' characters
        4 => format!("{}--{}", gen_decimal(u, 8), gen_decimal(u, 8)),
        // empty-ish
        _ => "-".to_string(),
    }
}

fn gen_structured_header(u: &mut Unstructured<'_>) -> String {
    // Bias towards "bytes=" (valid unit), but still allow invalid units.
    let unit_kind: u8 = u.arbitrary().unwrap_or(0);
    let mut out = match unit_kind % 4 {
        0 => "bytes=".to_string(),
        1 => "BYTES=".to_string(),
        2 => "bytes = ".to_string(),
        _ => "nope=".to_string(),
    };

    // Up to slightly above MAX_RANGE_SPECS to stress the cap.
    let specs: usize = u.int_in_range(0usize..=1100).unwrap_or(0);
    for i in 0..specs {
        if i != 0 {
            out.push(',');
        }
        // Optional whitespace around each part.
        if u.arbitrary::<bool>().unwrap_or(false) {
            out.push(' ');
        }
        out.push_str(&gen_range_spec(u));
        if u.arbitrary::<bool>().unwrap_or(false) {
            out.push(' ');
        }
        // Stop if we've already exceeded the max header length by a decent margin; we'll handle
        // final sizing (pad/truncate) after generation.
        if out.len() > MAX_RANGE_HEADER_LEN.saturating_add(512) {
            break;
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Resource length for `resolve_ranges`.
    let len: u64 = u.arbitrary().unwrap_or(0);
    let coalesce: bool = u.arbitrary().unwrap_or(false);

    // How to construct the header:
    // - 0: raw bytes
    // - 1: repeated bytes sized near the length cap
    // - 2: structured-ish Range header, optionally padded/truncated to near the cap
    let mode: u8 = u.arbitrary().unwrap_or(0);

    let rest_len = u.len();
    let rest = u.bytes(rest_len).unwrap_or(&[]);

    let header = match mode % 3 {
        0 => String::from_utf8_lossy(rest).into_owned(),
        1 => {
            let mut bytes = rest.to_vec();
            if bytes.is_empty() {
                bytes.push(b'A');
            }
            let seed: i16 = u.arbitrary().unwrap_or(0);
            let target_len = choose_target_len(seed, MAX_RANGE_HEADER_LEN, MAX_RANGE_HEADER_LEN + 512);
            while bytes.len() < target_len {
                let need = target_len - bytes.len();
                let take = need.min(rest.len().max(1));
                let src = if rest.is_empty() { &[b'A'][..] } else { &rest[..take.min(rest.len())] };
                bytes.extend_from_slice(src);
            }
            bytes.truncate(target_len);
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => {
            let mut s = gen_structured_header(&mut u);
            // Adjust total length near the cap via padding/truncation. Padding uses spaces, which
            // are trimmed by the parser but still count towards the pre-trim length cap.
            let seed: i16 = u.arbitrary().unwrap_or(0);
            let target_len = choose_target_len(seed, MAX_RANGE_HEADER_LEN, MAX_RANGE_HEADER_LEN + 512);
            if s.len() < target_len {
                s.extend(std::iter::repeat(' ').take(target_len - s.len()));
            } else {
                s.truncate(target_len);
            }
            s
        }
    };

    if let Ok(specs) = parse_range_header(&header) {
        let _ = resolve_ranges(&specs, len, coalesce);
    } else {
        // Still exercise parsing for error paths; the oracle is "must not panic".
        let _ = parse_range_header(&header);
    }
});
