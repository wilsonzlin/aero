//! HTTP `Range` handling for disk image streaming.
//!
//! Aero's disk streaming contract only requires **single-range** `bytes` requests.
//! Multi-range requests are rejected (see `docs/16-disk-image-streaming-auth.md`).
//!
//! Parsing and RFC 7233/9110 resolution are delegated to `aero-http-range` so we
//! keep one hardened implementation (header size limits, range-count limits,
//! overflow-safe `u64` parsing) across all services.

use aero_http_range as http_range;

pub use http_range::{ByteRangeSpec, RangeParseError, MAX_RANGE_HEADER_LEN, MAX_RANGE_SPECS};

/// An inclusive byte range (`start..=end`).
pub type ByteRange = http_range::ResolvedByteRange;

#[derive(Debug, Clone, Copy)]
pub struct RangeOptions {
    /// Maximum allowed bytes served for a single range request.
    pub max_total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RangeResolveError {
    #[error("multi-range requests are not supported")]
    MultiRangeNotSupported,
    #[error("range is unsatisfiable")]
    Unsatisfiable,
    #[error("range request too large")]
    TooManyBytes,
}

/// Parse an HTTP `Range` header value.
///
/// Returns `Ok(None)` when the range-unit is not `bytes` (RFC 9110 says it
/// should be ignored in that case).
pub fn parse_range_header(value: &str) -> Result<Option<Vec<ByteRangeSpec>>, RangeParseError> {
    match http_range::parse_range_header(value) {
        Ok(specs) => Ok(Some(specs)),
        Err(RangeParseError::UnsupportedUnit) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Resolve a **single** `bytes` range request against the representation length.
///
/// Returned ranges use inclusive `start..=end` offsets.
pub fn resolve_range(
    specs: &[ByteRangeSpec],
    len: u64,
    options: RangeOptions,
) -> Result<ByteRange, RangeResolveError> {
    if specs.len() != 1 {
        return Err(RangeResolveError::MultiRangeNotSupported);
    }

    let resolved = http_range::resolve_ranges(specs, len, false)
        .map_err(|_| RangeResolveError::Unsatisfiable)?;
    debug_assert_eq!(resolved.len(), 1);
    let range = resolved[0];

    if range.len() > options.max_total_bytes {
        return Err(RangeResolveError::TooManyBytes);
    }

    Ok(range)
}
