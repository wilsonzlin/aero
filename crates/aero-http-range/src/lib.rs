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
            .and_then(|delta| delta.checked_add(1))
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
    // Avoid scanning very long strings (potential DoS). `u64::MAX` is 20 digits.
    if s.len() > 20 {
        return Err(RangeParseError::InvalidNumber);
    }

    let mut value: u64 = 0;
    if s.is_empty() {
        return Err(RangeParseError::InvalidNumber);
    }
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
