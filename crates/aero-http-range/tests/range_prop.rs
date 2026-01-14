#![cfg(not(target_arch = "wasm32"))]

use proptest::prelude::*;

use aero_http_range::{
    parse_range_header, resolve_ranges, ByteRangeSpec, RangeParseError, RangeResolveError,
    ResolvedByteRange, MAX_RANGE_SPECS,
};

fn ows() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(""), Just(" "), Just("\t"), Just("  "), Just(" \t "),]
}

fn valid_spec() -> impl Strategy<Value = (ByteRangeSpec, String)> {
    // Generate a spec along with a string that should parse back into it (up to
    // whitespace differences).
    prop_oneof![
        // start-end
        (0u64..10_000u64, 0u64..10_000u64, ows(), ows(), ows(), ows()).prop_filter_map(
            "end must be >= start",
            |(start, end, ws0, ws1, ws2, ws3)| {
                if end < start {
                    return None;
                }
                let s = format!("{ws0}{start}{ws1}-{ws2}{end}{ws3}");
                Some((ByteRangeSpec::FromTo { start, end }, s))
            },
        ),
        // start-
        (0u64..10_000u64, ows(), ows(), ows()).prop_map(|(start, ws0, ws1, ws2)| {
            let s = format!("{ws0}{start}{ws1}-{ws2}");
            (ByteRangeSpec::From { start }, s)
        }),
        // -suffix
        (1u64..10_000u64, ows(), ows(), ows()).prop_map(|(len, ws0, ws1, ws2)| {
            let s = format!("{ws0}-{ws1}{len}{ws2}");
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

fn valid_header_with_specs() -> impl Strategy<Value = (Vec<ByteRangeSpec>, String)> {
    (
        ows(),
        prop::collection::vec(valid_spec(), 1..20),
        ows(),
        ows(),
    )
        .prop_map(|(ws0, specs, ws1, ws2)| {
            let mut out_specs = Vec::with_capacity(specs.len());
            let mut out_strings = Vec::with_capacity(specs.len());
            for (spec, s) in specs {
                out_specs.push(spec);
                out_strings.push(s);
            }
            let header = format!("{ws0}bytes{ws1}={ws2}{}", out_strings.join(","));
            (out_specs, header)
        })
}

fn arbitrary_spec() -> impl Strategy<Value = ByteRangeSpec> {
    prop_oneof![
        (any::<u64>(), any::<u64>()).prop_map(|(start, end)| ByteRangeSpec::FromTo { start, end }),
        any::<u64>().prop_map(|start| ByteRangeSpec::From { start }),
        any::<u64>().prop_map(|len| ByteRangeSpec::Suffix { len }),
    ]
}

fn spec_to_model_range(spec: ByteRangeSpec, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }

    match spec {
        ByteRangeSpec::FromTo { start, end } => {
            if start >= len {
                return None;
            }
            let end = end.min(len - 1);
            if end < start {
                return None;
            }
            Some((start, end))
        }
        ByteRangeSpec::From { start } => {
            if start >= len {
                return None;
            }
            Some((start, len - 1))
        }
        ByteRangeSpec::Suffix { len: suffix_len } => {
            if suffix_len == 0 {
                return None;
            }
            if suffix_len >= len {
                Some((0, len - 1))
            } else {
                Some((len - suffix_len, len - 1))
            }
        }
    }
}

fn coalesce_model(mut ranges: Vec<ResolvedByteRange>) -> Vec<ResolvedByteRange> {
    ranges.sort_by_key(|r| r.start);
    if ranges.is_empty() {
        return ranges;
    }

    let mut out = Vec::with_capacity(ranges.len());
    let mut cur = ranges[0];
    for r in ranges.into_iter().skip(1) {
        if r.start <= cur.end.saturating_add(1) {
            cur.end = cur.end.max(r.end);
        } else {
            out.push(cur);
            cur = r;
        }
    }
    out.push(cur);
    out
}

proptest! {
    #![proptest_config(ProptestConfig {
        // These are integration tests (in `tests/`), so proptest's default
        // `FileFailurePersistence::SourceParallel` can't reliably locate the crate root
        // for storing regression files. Disable persistence to avoid noisy warnings.
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

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
                    prop_assert!(len > 0);
                    prop_assert!(r.end < len);
                    let expected_len = r.end - r.start + 1;
                    prop_assert_eq!(r.len(), expected_len);
                    prop_assert!(r.len() > 0);
                }

                // Sorted & non-overlapping when coalescing enabled.
                for pair in ranges.windows(2) {
                    let a = pair[0];
                    let b = pair[1];
                    prop_assert!(a.start <= b.start);
                    // Coalescing should remove overlaps *and* adjacency.
                    prop_assert!(a.end.saturating_add(1) < b.start);
                }

                // Total length equals sum(end-start+1).
                let sum = ranges.iter().map(|r| r.len()).sum::<u64>();
                let manual = ranges.iter().map(|r| r.end - r.start + 1).sum::<u64>();
                prop_assert_eq!(sum, manual);
            }
        }
    }

    #[test]
    fn parse_roundtrips_for_generated_specs((expected, header) in valid_header_with_specs()) {
        let parsed = parse_range_header(&header).expect("generated header must parse");
        prop_assert_eq!(parsed, expected);
    }

    // `resolve_ranges` should never panic, even when given syntactically-invalid specs
    // directly (e.g. suffix len 0 or end < start).
    #[test]
    fn resolve_never_panics_on_arbitrary_specs(
        specs in prop::collection::vec(arbitrary_spec(), 0..50),
        len in 0u64..2_000u64,
        coalesce in any::<bool>(),
    ) {
        let resolved = resolve_ranges(&specs, len, coalesce);
        match resolved {
            Err(RangeResolveError::Unsatisfiable) => {
                // OK: len == 0 or all ranges dropped.
            }
            Ok(ranges) => {
                prop_assert!(len > 0);
                prop_assert!(!ranges.is_empty());

                for r in &ranges {
                    prop_assert!(r.start <= r.end);
                    prop_assert!(r.end < len);
                    let expected_len = r.end - r.start + 1;
                    prop_assert_eq!(r.len(), expected_len);
                    prop_assert!(r.len() > 0);
                }

                if coalesce {
                    for pair in ranges.windows(2) {
                        let a = pair[0];
                        let b = pair[1];
                        prop_assert!(a.start <= b.start);
                        prop_assert!(a.end.saturating_add(1) < b.start);
                    }
                }
            }
        }
    }

    #[test]
    fn resolve_matches_range_level_model(
        specs in prop::collection::vec(arbitrary_spec(), 0..50),
        len in 0u64..2_000u64,
    ) {
        let mut expected = Vec::new();
        if len > 0 {
            for &spec in &specs {
                if let Some((start, end)) = spec_to_model_range(spec, len) {
                    expected.push(ResolvedByteRange { start, end });
                }
            }
        }

        let actual = resolve_ranges(&specs, len, false);
        match actual {
            Err(RangeResolveError::Unsatisfiable) => {
                prop_assert!(expected.is_empty());
            }
            Ok(ranges) => {
                prop_assert!(!expected.is_empty());
                prop_assert_eq!(&ranges, &expected);
            }
        }

        let actual = resolve_ranges(&specs, len, true);
        match actual {
            Err(RangeResolveError::Unsatisfiable) => {
                prop_assert!(expected.is_empty());
            }
            Ok(ranges) => {
                prop_assert!(!expected.is_empty());
                let expected_coalesced = coalesce_model(expected.clone());
                prop_assert_eq!(&ranges, &expected_coalesced);
            }
        }
    }

    // Compare `resolve_ranges` against a simple byte-level model implementation, for
    // small lengths (so we can enumerate all covered bytes).
    #[test]
    fn resolve_matches_byte_level_model(
        specs in prop::collection::vec(arbitrary_spec(), 0..50),
        len in 0u64..2_000u64,
        coalesce in any::<bool>(),
    ) {
        let expected = if len == 0 {
            Vec::new()
        } else {
            let mut covered = vec![false; len as usize];
            for &spec in &specs {
                if let Some((start, end)) = spec_to_model_range(spec, len) {
                    for i in (start as usize)..=(end as usize) {
                        covered[i] = true;
                    }
                }
            }
            covered
        };

        let expected_any = expected.iter().any(|&b| b);

        let resolved = resolve_ranges(&specs, len, coalesce);
        match resolved {
            Err(RangeResolveError::Unsatisfiable) => {
                prop_assert!(!expected_any);
            }
            Ok(ranges) => {
                prop_assert!(expected_any);
                prop_assert!(len > 0);
                prop_assert!(!ranges.is_empty());

                let mut actual = vec![false; len as usize];
                for r in &ranges {
                    prop_assert!(r.start <= r.end);
                    prop_assert!(r.end < len);
                    for i in (r.start as usize)..=(r.end as usize) {
                        actual[i] = true;
                    }
                }

                prop_assert_eq!(actual, expected);

                if coalesce {
                    for pair in ranges.windows(2) {
                        let a = pair[0];
                        let b = pair[1];
                        prop_assert!(a.start <= b.start);
                        prop_assert!(a.end.saturating_add(1) < b.start);
                    }
                }
            }
        }
    }

    // Coalescing should not change the *set* of bytes covered by the resolved
    // ranges; it should only merge/sort them.
    #[test]
    fn coalescing_preserves_union(header in valid_header(), len in 0u64..2_000u64) {
        let specs = parse_range_header(&header).expect("valid header must parse");

        let a = resolve_ranges(&specs, len, false);
        let b = resolve_ranges(&specs, len, true);

        match (a, b) {
            (Err(RangeResolveError::Unsatisfiable), Err(RangeResolveError::Unsatisfiable)) => {}
            (Ok(a), Ok(b)) => {
                prop_assert!(len > 0);
                let len_usize = len as usize;
                let mut cover_a = vec![false; len_usize];
                let mut cover_b = vec![false; len_usize];

                for r in &a {
                    for i in (r.start as usize)..=(r.end as usize) {
                        cover_a[i] = true;
                    }
                }
                for r in &b {
                    for i in (r.start as usize)..=(r.end as usize) {
                        cover_b[i] = true;
                    }
                }

                prop_assert_eq!(cover_a, cover_b);
            }
            other => prop_assert!(false, "coalesce changed satisfiable-ness: {other:?}"),
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
    assert!(
        matches!(err, RangeParseError::TooManyRanges { .. }),
        "{err:?}"
    );
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

#[test]
fn accepts_zero_padded_numbers_longer_than_20_digits() {
    let start = format!("{}1", "0".repeat(25));
    let end = format!("{}2", "0".repeat(25));
    let suffix = format!("{}3", "0".repeat(25));
    let header = format!("bytes={start}-{end},-{suffix}");

    let specs = parse_range_header(&header).expect("zero padded values should parse");
    assert_eq!(
        specs,
        vec![
            ByteRangeSpec::FromTo { start: 1, end: 2 },
            ByteRangeSpec::Suffix { len: 3 },
        ]
    );
}

#[test]
fn accepts_leading_zeros() {
    let header = "bytes=0000-0001,0002-0003,-0004";
    let specs = parse_range_header(header).expect("leading zeros should be tolerated");
    assert_eq!(
        specs,
        vec![
            ByteRangeSpec::FromTo { start: 0, end: 1 },
            ByteRangeSpec::FromTo { start: 2, end: 3 },
            ByteRangeSpec::Suffix { len: 4 },
        ]
    );

    let resolved = resolve_ranges(&specs, 100, true).unwrap();
    assert_eq!(resolved[0].start, 0);
    assert_eq!(resolved[0].end, 3);
    assert_eq!(resolved[1].len(), 4);
}
