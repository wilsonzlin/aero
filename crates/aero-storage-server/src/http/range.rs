use std::num::ParseIntError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        // `end` is inclusive.
        debug_assert!(self.start <= self.end);
        self.end - self.start + 1
    }

    pub fn is_empty(self) -> bool {
        self.start > self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRangeSpec {
    FromTo { start: u64, end: Option<u64> },
    Suffix { suffix_len: u64 },
}

#[derive(Debug, Clone, Copy)]
pub struct RangeOptions {
    pub max_ranges: usize,
    pub max_total_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RangeParseError {
    #[error("invalid Range header")]
    Invalid,
}

#[derive(Debug, thiserror::Error)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_range() {
        let specs = parse_range_header("bytes=0-0").unwrap().unwrap();
        assert_eq!(
            specs,
            vec![ByteRangeSpec::FromTo {
                start: 0,
                end: Some(0)
            }]
        );
    }

    #[test]
    fn parse_suffix_range() {
        let specs = parse_range_header("bytes=-123").unwrap().unwrap();
        assert_eq!(specs, vec![ByteRangeSpec::Suffix { suffix_len: 123 }]);
    }

    #[test]
    fn resolve_clamps_end_to_len() {
        let ranges = resolve_ranges(
            &[ByteRangeSpec::FromTo {
                start: 0,
                end: Some(999),
            }],
            10,
            RangeOptions {
                max_ranges: 16,
                max_total_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(ranges, vec![ByteRange { start: 0, end: 9 }]);
    }

    #[test]
    fn resolve_suffix_for_large_lengths() {
        let len = 25 * 1024 * 1024 * 1024u64;
        let ranges = resolve_ranges(
            &[ByteRangeSpec::Suffix { suffix_len: 1 }],
            len,
            RangeOptions {
                max_ranges: 16,
                max_total_bytes: 1024,
            },
        )
        .unwrap();
        assert_eq!(
            ranges,
            vec![ByteRange {
                start: len - 1,
                end: len - 1
            }]
        );
    }
}
