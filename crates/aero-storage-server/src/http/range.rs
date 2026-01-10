//! HTTP `Range` (RFC 9110) parsing and satisfaction for the `bytes` range-unit.
//!
//! This module is designed for serving very large (multi-GB) files such as disk
//! images. All offsets use `u64` end-to-end.
//!
//! # Invalid input policy
//!
//! - Unknown range units return `Ok(None)` (RFC 9110 says they should be ignored).
//! - Syntactically invalid `bytes` ranges return `Err(RangeParseError::Invalid)`.
//!   Callers can choose whether to treat this as "ignore Range" or a client
//!   error (e.g. `400`) depending on their needs.

use std::cmp;
use std::num::ParseIntError;

/// An inclusive byte range (`start..=end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Length of this range in bytes.
    pub fn len(self) -> u64 {
        // `end` is inclusive.
        debug_assert!(self.start <= self.end);
        self.end - self.start + 1
    }

    pub fn is_empty(self) -> bool {
        self.start > self.end
    }
}

/// A range-specifier from RFC 9110.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRangeSpec {
    /// `first-last` or `first-`.
    FromTo { start: u64, end: Option<u64> },
    /// `-suffix-length`.
    Suffix { suffix_len: u64 },
}

#[derive(Debug, Clone, Copy)]
pub struct RangeOptions {
    pub max_ranges: usize,
    pub max_total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RangeParseError {
    #[error("invalid Range header")]
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RangeResolveError {
    #[error("no satisfiable ranges")]
    NoSatisfiableRanges,
    #[error("too many ranges")]
    TooManyRanges,
    #[error("range request too large")]
    TooManyBytes,
}

/// Parse an HTTP `Range` header value.
///
/// Returns `Ok(None)` when the range-unit is not `bytes` (RFC 9110 says it
/// should be ignored in that case).
pub fn parse_range_header(value: &str) -> Result<Option<Vec<ByteRangeSpec>>, RangeParseError> {
    let value = value.trim();
    let (unit, rest) = value.split_once('=').ok_or(RangeParseError::Invalid)?;
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return Ok(None);
    }

    let mut out = Vec::new();
    for part in rest.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(RangeParseError::Invalid);
        }

        if let Some(suffix) = part.strip_prefix('-') {
            let suffix_len = parse_u64(suffix)?;
            out.push(ByteRangeSpec::Suffix { suffix_len });
            continue;
        }

        let (start, end) = part.split_once('-').ok_or(RangeParseError::Invalid)?;
        let start = parse_u64(start)?;
        let end = if end.is_empty() {
            None
        } else {
            Some(parse_u64(end)?)
        };

        if matches!(end, Some(end) if start > end) {
            // First-byte-pos must be <= last-byte-pos when present.
            return Err(RangeParseError::Invalid);
        }

        out.push(ByteRangeSpec::FromTo { start, end });
    }

    if out.is_empty() {
        return Err(RangeParseError::Invalid);
    }

    Ok(Some(out))
}

fn parse_u64(s: &str) -> Result<u64, RangeParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(RangeParseError::Invalid);
    }
    s.parse::<u64>()
        .map_err(|_e: ParseIntError| RangeParseError::Invalid)
}

/// Resolve parsed ranges against a representation length.
///
/// Returned ranges use inclusive `start..=end` offsets.
pub fn resolve_ranges(
    specs: &[ByteRangeSpec],
    len: u64,
    options: RangeOptions,
) -> Result<Vec<ByteRange>, RangeResolveError> {
    if specs.len() > options.max_ranges {
        return Err(RangeResolveError::TooManyRanges);
    }

    let mut resolved = Vec::new();
    for spec in specs {
        if let Some(r) = resolve_one(*spec, len) {
            resolved.push(r);
        }
    }

    if resolved.is_empty() {
        return Err(RangeResolveError::NoSatisfiableRanges);
    }

    if resolved.len() > options.max_ranges {
        return Err(RangeResolveError::TooManyRanges);
    }

    let mut total: u64 = 0;
    for r in &resolved {
        total = total
            .checked_add(r.len())
            .ok_or(RangeResolveError::TooManyBytes)?;
        if total > options.max_total_bytes {
            return Err(RangeResolveError::TooManyBytes);
        }
    }

    Ok(resolved)
}

fn resolve_one(spec: ByteRangeSpec, len: u64) -> Option<ByteRange> {
    if len == 0 {
        return None;
    }

    match spec {
        ByteRangeSpec::FromTo { start, end } => {
            if start >= len {
                return None;
            }
            let mut end = end.unwrap_or(len - 1);
            if end >= len {
                end = len - 1;
            }
            if start > end {
                return None;
            }
            Some(ByteRange { start, end })
        }
        ByteRangeSpec::Suffix { suffix_len } => {
            if suffix_len == 0 {
                return None;
            }
            if suffix_len >= len {
                return Some(ByteRange {
                    start: 0,
                    end: len - 1,
                });
            }
            Some(ByteRange {
                start: len - suffix_len,
                end: len - 1,
            })
        }
    }
}

/// Coalesce overlapping or adjacent ranges in-place.
///
/// This is an optional normalization step that can be useful to avoid repeated
/// I/O if the client sends overlapping/adjacent ranges.
pub fn coalesce_ranges(ranges: &mut Vec<ByteRange>) {
    if ranges.len() <= 1 {
        return;
    }

    ranges.sort_by_key(|r| r.start);

    let mut out = Vec::with_capacity(ranges.len());
    for r in ranges.iter().copied() {
        match out.last_mut() {
            None => out.push(r),
            Some(prev) => {
                let merge_limit = prev.end.saturating_add(1);
                if r.start <= merge_limit {
                    prev.end = cmp::max(prev.end, r.end);
                } else {
                    out.push(r);
                }
            }
        }
    }

    *ranges = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> RangeOptions {
        RangeOptions {
            max_ranges: 16,
            max_total_bytes: u64::MAX,
        }
    }

    #[test]
    fn parse_unknown_unit_is_ignored() {
        assert_eq!(parse_range_header("items=0-1").unwrap(), None);
    }

    #[test]
    fn parse_rejects_invalid_syntax() {
        assert!(parse_range_header("bytes=a-b").is_err());
        assert!(parse_range_header("bytes=3-2").is_err());
    }

    #[test]
    fn resolve_table_driven() {
        struct Case {
            header: &'static str,
            len: u64,
            expected: Result<Vec<ByteRange>, RangeResolveError>,
        }

        let cases = vec![
            Case {
                header: "bytes=0-0",
                len: 10,
                expected: Ok(vec![ByteRange { start: 0, end: 0 }]),
            },
            Case {
                header: "bytes=0-",
                len: 10,
                expected: Ok(vec![ByteRange { start: 0, end: 9 }]),
            },
            Case {
                header: "bytes=5-20",
                len: 10,
                expected: Ok(vec![ByteRange { start: 5, end: 9 }]),
            },
            Case {
                header: "bytes=-3",
                len: 10,
                expected: Ok(vec![ByteRange { start: 7, end: 9 }]),
            },
            Case {
                header: "bytes=10-",
                len: 10,
                expected: Err(RangeResolveError::NoSatisfiableRanges),
            },
            Case {
                header: "bytes=0-0,2-2",
                len: 3,
                expected: Ok(vec![
                    ByteRange { start: 0, end: 0 },
                    ByteRange { start: 2, end: 2 },
                ]),
            },
        ];

        for case in cases {
            let specs = parse_range_header(case.header).unwrap().unwrap();
            let actual = resolve_ranges(&specs, case.len, opts());
            assert_eq!(actual, case.expected, "header={}", case.header);
        }
    }

    #[test]
    fn supports_optional_whitespace() {
        let specs = parse_range_header(" bytes = 0-0, 2-4, -1 ").unwrap().unwrap();
        let ranges = resolve_ranges(&specs, 10, opts()).unwrap();
        assert_eq!(
            ranges,
            vec![
                ByteRange { start: 0, end: 0 },
                ByteRange { start: 2, end: 4 },
                ByteRange { start: 9, end: 9 }
            ]
        );
    }

    #[test]
    fn suffix_larger_than_len_selects_whole_representation() {
        let specs = parse_range_header("bytes=-500").unwrap().unwrap();
        let ranges = resolve_ranges(&specs, 10, opts()).unwrap();
        assert_eq!(ranges, vec![ByteRange { start: 0, end: 9 }]);
    }

    #[test]
    fn large_offsets_round_trip_without_truncation() {
        let len = 5_000_000_000u64;
        let specs = parse_range_header("bytes=4294967296-4294967400")
            .unwrap()
            .unwrap();
        let ranges = resolve_ranges(&specs, len, opts()).unwrap();
        assert_eq!(
            ranges,
            vec![ByteRange {
                start: 4_294_967_296,
                end: 4_294_967_400,
            }]
        );
    }

    #[test]
    fn coalesce_merges_adjacent_or_overlapping() {
        let mut ranges = vec![
            ByteRange { start: 0, end: 0 },
            ByteRange { start: 1, end: 2 },
            ByteRange { start: 4, end: 4 },
            ByteRange { start: 3, end: 3 },
        ];
        coalesce_ranges(&mut ranges);
        assert_eq!(ranges, vec![ByteRange { start: 0, end: 4 }]);
    }
}
