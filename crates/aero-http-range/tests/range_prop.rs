use proptest::prelude::*;

use aero_http_range::{
    parse_range_header, resolve_ranges, ByteRangeSpec, RangeParseError, RangeResolveError,
    MAX_RANGE_SPECS,
};

fn ows() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(""), Just(" "), Just("\t"), Just("  "), Just(" \t "),]
}

fn valid_spec() -> impl Strategy<Value = (ByteRangeSpec, String)> {
    // Generate a spec along with a string that should parse back into it (up to
    // whitespace differences).
    prop_oneof![
        // start-end
        (0u64..10_000u64, 0u64..10_000u64, ows(), ows(), ows()).prop_filter_map(
            "end must be >= start",
            |(start, end, ws1, ws2, ws3)| {
                if end < start {
                    return None;
                }
                let s = format!("{start}{ws1}-{ws2}{end}{ws3}");
                Some((ByteRangeSpec::FromTo { start, end }, s))
            },
        ),
        // start-
        (0u64..10_000u64, ows(), ows()).prop_map(|(start, ws1, ws2)| {
            let s = format!("{start}{ws1}-{ws2}");
            (ByteRangeSpec::From { start }, s)
        }),
        // -suffix
        (1u64..10_000u64, ows(), ows()).prop_map(|(len, ws1, ws2)| {
            let s = format!("-{ws1}{len}{ws2}");
            (ByteRangeSpec::Suffix { len }, s)
        }),
    ]
}

fn valid_header() -> impl Strategy<Value = String> {
    (
        ows(),
        prop::collection::vec(valid_spec().prop_map(|(_, s)| s), 1..20),
        ows(),
        ows(),
    )
        .prop_map(|(ws0, specs, ws1, ws2)| format!("{ws0}bytes{ws1}={ws2}{}", specs.join(",")))
}

proptest! {
    // Parser should never panic on arbitrary inputs.
    #[test]
    fn parse_never_panics(input in ".*") {
        std::panic::catch_unwind(|| {
            let _ = parse_range_header(&input);
        }).expect("parse_range_header panicked");
    }

    // Generated syntactically-valid headers should parse, and resolution should
    // satisfy invariants when it succeeds.
    #[test]
    fn resolve_invariants(header in valid_header(), len in 0u64..20_000u64) {
        let specs = parse_range_header(&header).expect("valid header must parse");

        let resolved = resolve_ranges(&specs, len, true);
        match resolved {
            Err(RangeResolveError::Unsatisfiable) => {
                // Acceptable for len == 0 or for all ranges starting beyond len.
            }
            Ok(ranges) => {
                // Invariants per-range.
                for r in &ranges {
                    prop_assert!(r.start <= r.end);
                    if len > 0 {
                        prop_assert!(r.end < len);
                    }
                }

                // Sorted & non-overlapping when coalescing enabled.
                for pair in ranges.windows(2) {
                    let a = pair[0];
                    let b = pair[1];
                    prop_assert!(a.start <= b.start);
                    prop_assert!(a.end < b.start);
                }

                // Total length equals sum(end-start+1).
                let sum = ranges.iter().map(|r| r.len()).sum::<u64>();
                let manual = ranges.iter().map(|r| r.end - r.start + 1).sum::<u64>();
                prop_assert_eq!(sum, manual);
            }
        }
    }
}

#[test]
fn rejects_too_many_ranges() {
    // 1001 ranges of length 1 each.
    let header = {
        let mut s = String::from("bytes=");
        for i in 0..(MAX_RANGE_SPECS as u64 + 1) {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("{i}-{i}"));
        }
        s
    };

    let err = parse_range_header(&header).unwrap_err();
    assert!(matches!(err, RangeParseError::TooManyRanges { .. }), "{err:?}");
}

#[test]
fn rejects_overflowing_numbers() {
    // u64::MAX + 1.
    let header = "bytes=18446744073709551616-18446744073709551616";
    let err = parse_range_header(header).unwrap_err();
    assert!(matches!(err, RangeParseError::InvalidNumber), "{err:?}");
}

#[test]
fn rejects_extremely_long_numbers() {
    let huge = "9".repeat(1024);
    let header = format!("bytes={huge}-{huge}");
    let err = parse_range_header(&header).unwrap_err();
    assert!(matches!(err, RangeParseError::InvalidNumber), "{err:?}");
}

#[test]
fn accepts_weird_whitespace() {
    let header = "bytes =\t 0 -\t 1 ,  2-3";
    let specs = parse_range_header(header).expect("whitespace should be tolerated");
    let resolved = resolve_ranges(&specs, 10, true).unwrap();
    assert_eq!(resolved[0].start, 0);
    assert_eq!(resolved[0].end, 3);
}

