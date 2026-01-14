#![forbid(unsafe_code)]

//! HTTP `Range` header parsing and resolution (RFC 7233).
//!
//! This module is intentionally defensive:
//! - It rejects headers that are too large (`MAX_RANGE_HEADER_LEN`).
//! - It rejects requests with too many ranges (`MAX_RANGE_SPECS`).
//! - It rejects integers that don't fit in `u64` without scanning arbitrarily
//!   long strings (overflow is detected via a digit-count guard).

use std::fmt;

/// Maximum accepted `Range` header length.
///
/// This is a DoS guard: attackers can send extremely large headers with
/// pathological whitespace or giant integers. Real HTTP stacks also enforce
/// header-size limits, but we keep this local cap as a last line of defense.
pub const MAX_RANGE_HEADER_LEN: usize = 8 * 1024;

/// Maximum number of comma-separated range specs we will parse.
///
/// This is a DoS guard: multi-range requests can otherwise trigger quadratic
/// processing, large allocations, or expensive multipart responses.
pub const MAX_RANGE_SPECS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeParseError {
    /// Header exceeded [`MAX_RANGE_HEADER_LEN`].
    HeaderTooLarge { len: usize, max: usize },
    /// Range unit was not `bytes`.
    UnsupportedUnit,
    /// General syntax error.
    InvalidSyntax,
    /// More than [`MAX_RANGE_SPECS`] comma-separated specs were encountered.
    TooManyRanges { max: usize },
    /// A number did not fit in `u64` or was otherwise invalid.
    InvalidNumber,
}

impl fmt::Display for RangeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeaderTooLarge { len, max } => write!(
                f,
                "Range header length {len} exceeds maximum accepted length {max}"
            ),
            Self::UnsupportedUnit => write!(f, "unsupported Range unit (expected bytes)"),
            Self::InvalidSyntax => write!(f, "invalid Range header syntax"),
            Self::TooManyRanges { max } => write!(f, "too many ranges (maximum {max})"),
            Self::InvalidNumber => write!(f, "invalid range number"),
        }
    }
}

impl std::error::Error for RangeParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeResolveError {
    /// The resource length is zero or none of the requested ranges overlap it.
    Unsatisfiable,
}

impl fmt::Display for RangeResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsatisfiable => write!(f, "range is unsatisfiable"),
        }
    }
}

impl std::error::Error for RangeResolveError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRangeSpec {
    /// `start-end` (inclusive).
    FromTo { start: u64, end: u64 },
    /// `start-` (until end of representation).
    From { start: u64 },
    /// `-suffix_len` (last `suffix_len` bytes).
    Suffix { len: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedByteRange {
    pub start: u64,
    pub end: u64, // inclusive
}

impl ResolvedByteRange {
    pub fn len(&self) -> u64 {
        self.end
            .checked_sub(self.start)
            .map(|delta| delta.saturating_add(1))
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Parse an HTTP `Range` header value (e.g. `bytes=0-99,200-`).
///
/// The parser is whitespace-tolerant and treats the unit as case-insensitive.
pub fn parse_range_header(header_value: &str) -> Result<Vec<ByteRangeSpec>, RangeParseError> {
    if header_value.len() > MAX_RANGE_HEADER_LEN {
        return Err(RangeParseError::HeaderTooLarge {
            len: header_value.len(),
            max: MAX_RANGE_HEADER_LEN,
        });
    }

    let trimmed = header_value.trim();

    // Split at the first '='. We allow optional whitespace around '='.
    let (unit, rest) = match trimmed.split_once('=') {
        Some((u, r)) => (u.trim(), r.trim()),
        None => return Err(RangeParseError::InvalidSyntax),
    };

    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(RangeParseError::UnsupportedUnit);
    }

    if rest.is_empty() {
        return Err(RangeParseError::InvalidSyntax);
    }

    let mut specs = Vec::new();
    for part in rest.split(',') {
        if specs.len() >= MAX_RANGE_SPECS {
            return Err(RangeParseError::TooManyRanges {
                max: MAX_RANGE_SPECS,
            });
        }

        let part = part.trim();
        if part.is_empty() {
            return Err(RangeParseError::InvalidSyntax);
        }

        let spec = parse_range_spec(part)?;
        specs.push(spec);
    }

    if specs.is_empty() {
        return Err(RangeParseError::InvalidSyntax);
    }

    Ok(specs)
}

fn parse_range_spec(spec: &str) -> Result<ByteRangeSpec, RangeParseError> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(RangeParseError::InvalidSyntax);
    }

    // suffix-byte-range-spec: "-" suffix-length
    if let Some(rest) = spec.strip_prefix('-') {
        let len_str = rest.trim();
        if len_str.is_empty() {
            return Err(RangeParseError::InvalidSyntax);
        }
        let len = parse_u64_decimal(len_str)?;
        if len == 0 {
            return Err(RangeParseError::InvalidSyntax);
        }
        return Ok(ByteRangeSpec::Suffix { len });
    }

    // byte-range-spec: first-byte-pos "-" [last-byte-pos]
    let (start_str, end_str) = match spec.split_once('-') {
        Some((s, e)) => (s.trim(), e.trim()),
        None => return Err(RangeParseError::InvalidSyntax),
    };

    if start_str.is_empty() {
        return Err(RangeParseError::InvalidSyntax);
    }
    let start = parse_u64_decimal(start_str)?;

    if end_str.is_empty() {
        return Ok(ByteRangeSpec::From { start });
    }

    let end = parse_u64_decimal(end_str)?;
    if end < start {
        return Err(RangeParseError::InvalidSyntax);
    }

    Ok(ByteRangeSpec::FromTo { start, end })
}

fn parse_u64_decimal(s: &str) -> Result<u64, RangeParseError> {
    // Avoid scanning very long strings (potential DoS).
    //
    // We keep two guards:
    // - hard cap to avoid pathological inputs that are still within the overall
    //   header size limit
    // - a tighter cap based on the maximum digits of `u64`, while still accepting
    //   values that are zero-padded beyond 20 digits (e.g. "000...001").
    const U64_MAX_DECIMAL_DIGITS: usize = 20; // `u64::MAX` is 20 digits.
    const MAX_DECIMAL_DIGITS: usize = 64;

    let mut s = s;
    if s.is_empty() || s.len() > MAX_DECIMAL_DIGITS {
        return Err(RangeParseError::InvalidNumber);
    }

    if s.len() > U64_MAX_DECIMAL_DIGITS {
        let extra = s.len() - U64_MAX_DECIMAL_DIGITS;
        if s.as_bytes()[..extra].iter().any(|&b| b != b'0') {
            return Err(RangeParseError::InvalidNumber);
        }
        s = &s[extra..];
    }

    let mut value: u64 = 0;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            return Err(RangeParseError::InvalidNumber);
        }
        let digit = (b - b'0') as u64;
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(digit))
            .ok_or(RangeParseError::InvalidNumber)?;
    }
    Ok(value)
}

/// Resolve a parsed list of byte range specs against a known representation length.
///
/// If `coalesce` is `true`, overlapping and adjacent ranges are merged and the
/// output is sorted by `start`.
pub fn resolve_ranges(
    specs: &[ByteRangeSpec],
    len: u64,
    coalesce: bool,
) -> Result<Vec<ResolvedByteRange>, RangeResolveError> {
    if len == 0 {
        return Err(RangeResolveError::Unsatisfiable);
    }

    let mut resolved = Vec::with_capacity(specs.len());
    for spec in specs {
        if let Some(r) = resolve_one(*spec, len) {
            resolved.push(r);
        }
    }

    if resolved.is_empty() {
        return Err(RangeResolveError::Unsatisfiable);
    }

    if coalesce {
        resolved.sort_by_key(|r| r.start);
        resolved = coalesce_sorted(resolved);
    }

    Ok(resolved)
}

fn resolve_one(spec: ByteRangeSpec, len: u64) -> Option<ResolvedByteRange> {
    debug_assert!(len > 0);

    match spec {
        ByteRangeSpec::FromTo { start, end } => {
            if start >= len {
                return None;
            }
            let end = end.min(len - 1);
            if end < start {
                return None;
            }
            Some(ResolvedByteRange { start, end })
        }
        ByteRangeSpec::From { start } => {
            if start >= len {
                return None;
            }
            Some(ResolvedByteRange {
                start,
                end: len - 1,
            })
        }
        ByteRangeSpec::Suffix { len: suffix_len } => {
            if suffix_len == 0 {
                return None;
            }
            if suffix_len >= len {
                Some(ResolvedByteRange {
                    start: 0,
                    end: len - 1,
                })
            } else {
                Some(ResolvedByteRange {
                    start: len - suffix_len,
                    end: len - 1,
                })
            }
        }
    }
}

fn coalesce_sorted(mut ranges: Vec<ResolvedByteRange>) -> Vec<ResolvedByteRange> {
    debug_assert!(!ranges.is_empty());

    let mut out = Vec::with_capacity(ranges.len());
    let mut cur = ranges[0];
    for r in ranges.drain(1..) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_resolved_invariants(ranges: &[ResolvedByteRange], len: u64) {
        assert!(len > 0, "len must be > 0 for resolved ranges");
        assert!(!ranges.is_empty(), "resolved ranges must not be empty");
        for r in ranges {
            assert!(
                r.start <= r.end,
                "resolved range start must be <= end: {r:?}"
            );
            assert!(r.end < len, "resolved range end must be < len: {r:?}");
            let expected_len = r.end - r.start + 1;
            assert_eq!(r.len(), expected_len, "resolved range len mismatch: {r:?}");
            assert!(!r.is_empty(), "resolved range len must be non-zero: {r:?}");
        }
    }

    fn assert_resolved_eq(ranges: &[ResolvedByteRange], expected: &[(u64, u64)], len: u64) {
        let expected = expected
            .iter()
            .map(|&(start, end)| ResolvedByteRange { start, end })
            .collect::<Vec<_>>();
        assert_eq!(ranges, expected);
        assert_resolved_invariants(ranges, len);
    }

    #[test]
    fn parse_valid_single_range_forms() {
        assert_eq!(
            parse_range_header("bytes=0-0").unwrap(),
            vec![ByteRangeSpec::FromTo { start: 0, end: 0 }]
        );
        assert_eq!(
            parse_range_header("bytes=0-99").unwrap(),
            vec![ByteRangeSpec::FromTo { start: 0, end: 99 }]
        );
        assert_eq!(
            parse_range_header("bytes=100-").unwrap(),
            vec![ByteRangeSpec::From { start: 100 }]
        );
        assert_eq!(
            parse_range_header("bytes=-1").unwrap(),
            vec![ByteRangeSpec::Suffix { len: 1 }]
        );
        assert_eq!(
            parse_range_header("bytes=-500").unwrap(),
            vec![ByteRangeSpec::Suffix { len: 500 }]
        );
    }

    #[test]
    fn parse_valid_multi_range_whitespace_tolerant() {
        assert_eq!(
            parse_range_header("bytes=0-0, 2-3").unwrap(),
            vec![
                ByteRangeSpec::FromTo { start: 0, end: 0 },
                ByteRangeSpec::FromTo { start: 2, end: 3 }
            ]
        );
        assert_eq!(
            parse_range_header("bytes = 0-1 , 4-5").unwrap(),
            vec![
                ByteRangeSpec::FromTo { start: 0, end: 1 },
                ByteRangeSpec::FromTo { start: 4, end: 5 }
            ]
        );
    }

    #[test]
    fn parse_invalid_syntax_and_number_cases() {
        // Empty rest.
        assert!(matches!(
            parse_range_header("bytes=").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));
        assert!(matches!(
            parse_range_header("bytes=   ").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));

        // Missing '-'.
        assert!(matches!(
            parse_range_header("bytes=0").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));

        // Extra commas / empty specs.
        for header in [
            "bytes=0-1,",
            "bytes=0-1,   ",
            "bytes=,0-1",
            "bytes=0-1,,2-3",
        ] {
            assert!(
                matches!(
                    parse_range_header(header).unwrap_err(),
                    RangeParseError::InvalidSyntax
                ),
                "expected InvalidSyntax for {header:?}"
            );
        }

        // Non-digit numbers.
        for header in [
            "bytes=a-1",
            "bytes=0-a",
            "bytes=0-1-2",
            "bytes=-1-2",
            "bytes=--1",
        ] {
            assert!(
                matches!(
                    parse_range_header(header).unwrap_err(),
                    RangeParseError::InvalidNumber
                ),
                "expected InvalidNumber for {header:?}"
            );
        }

        // end < start.
        assert!(matches!(
            parse_range_header("bytes=5-3").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));

        // -0 suffix is explicitly invalid.
        assert!(matches!(
            parse_range_header("bytes=-0").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));
        assert!(matches!(
            parse_range_header("bytes=-0000").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));
    }

    #[test]
    fn parse_rejects_unsupported_unit() {
        assert!(matches!(
            parse_range_header("items=0-1").unwrap_err(),
            RangeParseError::UnsupportedUnit
        ));
    }

    #[test]
    fn parse_unit_is_case_insensitive_and_requires_equals() {
        assert_eq!(
            parse_range_header("ByTeS=0-1").unwrap(),
            vec![ByteRangeSpec::FromTo { start: 0, end: 1 }]
        );
        assert!(matches!(
            parse_range_header("bytes 0-1").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));

        // Extra '=' after the unit should be treated as an invalid number in the first spec.
        assert!(matches!(
            parse_range_header("bytes==0-1").unwrap_err(),
            RangeParseError::InvalidNumber
        ));
    }

    #[test]
    fn parse_accepts_u64_max_and_resolution_clamps_correctly() {
        let max = u64::MAX.to_string();

        // end == u64::MAX parses, but is clamped to len-1 during resolution.
        let header = format!("bytes=0-{max}");
        let specs = parse_range_header(&header).unwrap();
        assert_eq!(
            specs,
            vec![ByteRangeSpec::FromTo {
                start: 0,
                end: u64::MAX
            }]
        );
        let resolved = resolve_ranges(&specs, u64::MAX, false).unwrap();
        assert_resolved_eq(&resolved, &[(0, u64::MAX - 1)], u64::MAX);

        // suffix == u64::MAX parses and resolves to the full representation for len == u64::MAX.
        let header = format!("bytes=-{max}");
        let specs = parse_range_header(&header).unwrap();
        assert_eq!(specs, vec![ByteRangeSpec::Suffix { len: u64::MAX }]);
        let resolved = resolve_ranges(&specs, u64::MAX, false).unwrap();
        assert_resolved_eq(&resolved, &[(0, u64::MAX - 1)], u64::MAX);
    }

    #[test]
    fn parse_accepts_u64_max_start_but_resolution_drops_start_equal_to_len() {
        let len = u64::MAX;
        let max = u64::MAX.to_string();

        // start == len should be dropped as unsatisfiable.
        let header = format!("bytes={max}-");
        let specs = parse_range_header(&header).unwrap();
        assert_eq!(specs, vec![ByteRangeSpec::From { start: u64::MAX }]);
        assert!(matches!(
            resolve_ranges(&specs, len, false),
            Err(RangeResolveError::Unsatisfiable)
        ));

        let header = format!("bytes={max}-{max}");
        let specs = parse_range_header(&header).unwrap();
        assert_eq!(
            specs,
            vec![ByteRangeSpec::FromTo {
                start: u64::MAX,
                end: u64::MAX
            }]
        );
        assert!(matches!(
            resolve_ranges(&specs, len, false),
            Err(RangeResolveError::Unsatisfiable)
        ));

        // end == u64::MAX is clamped to len-1.
        let start = u64::MAX - 1;
        let header = format!("bytes={start}-{max}");
        let specs = parse_range_header(&header).unwrap();
        let resolved = resolve_ranges(&specs, len, false).unwrap();
        assert_resolved_eq(&resolved, &[(u64::MAX - 1, u64::MAX - 1)], len);
    }

    #[test]
    fn parse_rejects_missing_suffix_length() {
        assert!(matches!(
            parse_range_header("bytes=-").unwrap_err(),
            RangeParseError::InvalidSyntax
        ));
    }

    #[test]
    fn dos_guard_rejects_header_over_max_len() {
        let header = "x".repeat(MAX_RANGE_HEADER_LEN + 1);
        match parse_range_header(&header).unwrap_err() {
            RangeParseError::HeaderTooLarge { len, max } => {
                assert_eq!(len, MAX_RANGE_HEADER_LEN + 1);
                assert_eq!(max, MAX_RANGE_HEADER_LEN);
            }
            other => panic!("expected HeaderTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn dos_guard_accepts_header_at_max_len() {
        let mut header = "bytes=0-0".to_string();
        header.extend(std::iter::repeat_n(' ', MAX_RANGE_HEADER_LEN - header.len()));
        assert_eq!(header.len(), MAX_RANGE_HEADER_LEN);

        let specs = parse_range_header(&header).unwrap();
        assert_eq!(specs, vec![ByteRangeSpec::FromTo { start: 0, end: 0 }]);
    }

    #[test]
    fn dos_guard_counts_untrimmed_whitespace_in_header_len() {
        // Even though the parser is whitespace-tolerant, the header-length DoS guard
        // is applied before trimming; this ensures excessively padded headers are
        // still rejected.
        let mut header = "bytes=0-0".to_string();
        header.extend(std::iter::repeat_n(' ', MAX_RANGE_HEADER_LEN + 1 - header.len()));
        assert_eq!(header.len(), MAX_RANGE_HEADER_LEN + 1);

        assert!(matches!(
            parse_range_header(&header).unwrap_err(),
            RangeParseError::HeaderTooLarge { .. }
        ));
    }

    #[test]
    fn dos_guard_rejects_very_long_integers_and_non_zero_prefix_overflow() {
        // parse_u64_decimal should reject numbers longer than its own scan cap.
        let too_long = "9".repeat(65);
        let header = format!("bytes={too_long}-{too_long}");
        assert!(matches!(
            parse_range_header(&header).unwrap_err(),
            RangeParseError::InvalidNumber
        ));

        // >20 digits with a non-zero prefix should be rejected without attempting to
        // parse into u64.
        let overflow_digits = format!("1{}", "0".repeat(20)); // 21 digits
        let header = format!("bytes={overflow_digits}-{overflow_digits}");
        assert!(matches!(
            parse_range_header(&header).unwrap_err(),
            RangeParseError::InvalidNumber
        ));
    }

    #[test]
    fn dos_guard_accepts_max_decimal_digits_when_zero_padded() {
        // parse_u64_decimal permits numbers longer than 20 digits if the extra prefix
        // digits are all zeros, up to MAX_DECIMAL_DIGITS (64).
        let max = u64::MAX.to_string(); // 20 digits
        let n = format!("{}{}", "0".repeat(44), max); // 64 digits total
        assert_eq!(n.len(), 64);

        let header = format!("bytes={n}-{n}");
        let specs = parse_range_header(&header).unwrap();
        assert_eq!(
            specs,
            vec![ByteRangeSpec::FromTo {
                start: u64::MAX,
                end: u64::MAX
            }]
        );
    }

    #[test]
    fn dos_guard_rejects_zero_padded_overflowing_u64() {
        // If the significant (last 20) digits overflow u64, we must still reject,
        // even if the number is heavily zero-padded.
        let overflow = "18446744073709551616"; // u64::MAX + 1
        let n = format!("{}{}", "0".repeat(44), overflow); // 64 digits total
        assert_eq!(n.len(), 64);

        let header = format!("bytes={n}-{n}");
        assert!(matches!(
            parse_range_header(&header).unwrap_err(),
            RangeParseError::InvalidNumber
        ));
    }

    #[test]
    fn dos_guard_rejects_too_many_ranges() {
        // 1001 ranges of length 1 each (should exceed MAX_RANGE_SPECS==1000).
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

        assert!(matches!(
            parse_range_header(&header).unwrap_err(),
            RangeParseError::TooManyRanges { .. }
        ));
    }

    #[test]
    fn dos_guard_accepts_exactly_max_range_specs() {
        // Exactly MAX_RANGE_SPECS should be accepted (guard is strictly "more than").
        let header = {
            let mut s = String::from("bytes=");
            for i in 0..(MAX_RANGE_SPECS as u64) {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!("{i}-{i}"));
            }
            s
        };
        let specs = parse_range_header(&header).unwrap();
        assert_eq!(specs.len(), MAX_RANGE_SPECS);
    }

    #[test]
    fn resolve_len_zero_is_unsatisfiable() {
        let specs = parse_range_header("bytes=0-0").unwrap();
        assert!(matches!(
            resolve_ranges(&specs, 0, false),
            Err(RangeResolveError::Unsatisfiable)
        ));
    }

    #[test]
    fn resolve_empty_specs_is_unsatisfiable() {
        assert!(matches!(
            resolve_ranges(&[], 10, false),
            Err(RangeResolveError::Unsatisfiable)
        ));
        assert!(matches!(
            resolve_ranges(&[], 10, true),
            Err(RangeResolveError::Unsatisfiable)
        ));
    }

    #[test]
    fn resolve_drops_invalid_suffix_len_zero_and_fromto_end_before_start() {
        // These specs cannot be produced by the parser (it rejects -0 and end < start),
        // but `resolve_ranges` should be defensive when given them directly.
        let specs = [
            ByteRangeSpec::Suffix { len: 0 },
            ByteRangeSpec::FromTo { start: 5, end: 3 },
        ];
        assert!(matches!(
            resolve_ranges(&specs, 10, false),
            Err(RangeResolveError::Unsatisfiable)
        ));

        let specs = [
            ByteRangeSpec::Suffix { len: 0 },
            ByteRangeSpec::FromTo { start: 0, end: 0 },
        ];
        let resolved = resolve_ranges(&specs, 10, false).unwrap();
        assert_resolved_eq(&resolved, &[(0, 0)], 10);
    }

    #[test]
    fn resolve_supports_large_lengths_without_overflow() {
        let len = u64::MAX;

        // Clamp end to len-1 without overflow.
        let specs = [ByteRangeSpec::FromTo {
            start: 0,
            end: u64::MAX,
        }];
        let resolved = resolve_ranges(&specs, len, false).unwrap();
        assert_resolved_eq(&resolved, &[(0, u64::MAX - 1)], len);

        // Suffix longer than (or equal to) the length yields the full range.
        let specs = [ByteRangeSpec::Suffix { len: u64::MAX }];
        let resolved = resolve_ranges(&specs, len, false).unwrap();
        assert_resolved_eq(&resolved, &[(0, u64::MAX - 1)], len);

        // Coalescing should merge a suffix range contained within a larger range.
        let specs = [
            ByteRangeSpec::FromTo {
                start: 0,
                end: u64::MAX,
            },
            ByteRangeSpec::Suffix { len: 1 },
        ];
        let resolved = resolve_ranges(&specs, len, true).unwrap();
        assert_resolved_eq(&resolved, &[(0, u64::MAX - 1)], len);
    }

    #[test]
    fn coalesce_sorted_handles_u64_max_end_without_overflow() {
        // `coalesce_sorted` uses `saturating_add(1)` to avoid overflow when testing
        // for adjacency. Ensure it merges correctly when the current end is u64::MAX.
        let ranges = vec![
            ResolvedByteRange {
                start: 0,
                end: u64::MAX,
            },
            ResolvedByteRange {
                start: u64::MAX,
                end: u64::MAX,
            },
        ];
        let merged = coalesce_sorted(ranges);
        assert_eq!(
            merged,
            vec![ResolvedByteRange {
                start: 0,
                end: u64::MAX
            }]
        );
    }

    #[test]
    fn resolve_drops_out_of_bounds_and_errors_when_none_remain() {
        let specs = parse_range_header("bytes=0-0,20-30").unwrap();
        let resolved = resolve_ranges(&specs, 10, false).unwrap();
        assert_resolved_eq(&resolved, &[(0, 0)], 10);

        let specs = parse_range_header("bytes=20-30").unwrap();
        assert!(matches!(
            resolve_ranges(&specs, 10, false),
            Err(RangeResolveError::Unsatisfiable)
        ));
    }

    #[test]
    fn resolve_clamps_end_and_suffix_longer_than_resource_is_full_range() {
        let specs = [ByteRangeSpec::FromTo { start: 5, end: 100 }];
        let resolved = resolve_ranges(&specs, 10, false).unwrap();
        assert_resolved_eq(&resolved, &[(5, 9)], 10);

        let specs = [ByteRangeSpec::Suffix { len: 500 }];
        let resolved = resolve_ranges(&specs, 10, false).unwrap();
        assert_resolved_eq(&resolved, &[(0, 9)], 10);
    }

    #[test]
    fn resolve_open_ended_from_resolves_to_end_and_drops_if_start_out_of_bounds() {
        let specs = [ByteRangeSpec::From { start: 7 }];
        let resolved = resolve_ranges(&specs, 10, false).unwrap();
        assert_resolved_eq(&resolved, &[(7, 9)], 10);

        let specs = [ByteRangeSpec::From { start: 10 }];
        assert!(matches!(
            resolve_ranges(&specs, 10, false),
            Err(RangeResolveError::Unsatisfiable)
        ));
    }

    #[test]
    fn resolve_no_coalesce_preserves_order_and_does_not_merge() {
        // When `coalesce=false`, resolution should preserve input order and should
        // not merge overlapping/adjacent ranges.
        let specs = [
            ByteRangeSpec::FromTo { start: 5, end: 6 },
            ByteRangeSpec::FromTo { start: 0, end: 1 },
            ByteRangeSpec::FromTo { start: 1, end: 2 }, // overlaps/adjacent with previous
        ];
        let resolved = resolve_ranges(&specs, 10, false).unwrap();
        assert_eq!(
            resolved,
            vec![
                ResolvedByteRange { start: 5, end: 6 },
                ResolvedByteRange { start: 0, end: 1 },
                ResolvedByteRange { start: 1, end: 2 },
            ]
        );
        assert_resolved_invariants(&resolved, 10);
    }

    #[test]
    fn resolve_coalesce_sorts_and_merges_overlapping_and_adjacent_ranges() {
        // Unsorted, overlapping and adjacent.
        let specs = [
            ByteRangeSpec::FromTo { start: 5, end: 6 },
            ByteRangeSpec::FromTo { start: 0, end: 2 },
            ByteRangeSpec::FromTo { start: 2, end: 4 },
            ByteRangeSpec::FromTo { start: 10, end: 12 },
        ];
        let resolved = resolve_ranges(&specs, 20, true).unwrap();
        assert_resolved_eq(&resolved, &[(0, 6), (10, 12)], 20);

        // Coalesced ranges must be strictly separated by at least 1 byte.
        for pair in resolved.windows(2) {
            let a = pair[0];
            let b = pair[1];
            assert!(a.start <= b.start);
            assert!(a.end.saturating_add(1) < b.start);
        }
    }

    #[test]
    fn rfc_7233_example_ranges() {
        // RFC 7233 §2.1 examples, resolved against a 10,000-byte representation.
        let len = 10_000u64;

        for (header, expected) in [
            ("bytes=0-499", &[(0, 499)][..]),
            ("bytes=500-999", &[(500, 999)][..]),
            ("bytes=-500", &[(9500, 9999)][..]),
            ("bytes=9500-", &[(9500, 9999)][..]),
            // Common multi-range example.
            ("bytes=0-0,-1", &[(0, 0), (9999, 9999)][..]),
        ] {
            let specs = parse_range_header(header).unwrap();
            let resolved = resolve_ranges(&specs, len, false).unwrap();
            assert_resolved_eq(&resolved, expected, len);
        }

        // Adjacent ranges can be coalesced into a single one.
        let specs = parse_range_header("bytes=0-499,500-999").unwrap();
        let resolved = resolve_ranges(&specs, len, true).unwrap();
        assert_resolved_eq(&resolved, &[(0, 999)], len);
    }
}
