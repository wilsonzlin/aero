use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{header, HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

use crate::store::ImageMeta;

pub fn etag_or_fallback(meta: &ImageMeta) -> String {
    meta.etag
        .clone()
        .unwrap_or_else(|| weak_etag_from_size_and_mtime(meta.size, meta.last_modified))
}

pub fn weak_etag_from_size_and_mtime(size: u64, mtime: Option<SystemTime>) -> String {
    let (sec, nsec) = mtime
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0));

    format!("W/\"{size:x}-{sec:x}-{nsec:x}\"")
}

pub fn last_modified_header_value(last_modified: Option<SystemTime>) -> Option<HeaderValue> {
    let last_modified = last_modified?;
    let s = httpdate::fmt_http_date(last_modified);
    Some(HeaderValue::from_str(&s).expect("http-date must be a valid header value"))
}

pub fn etag_for_image_list(entries: &[(String, ImageMeta)]) -> HeaderValue {
    let mut entries = entries.to_vec();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut h = Sha256::new();
    for (image_id, meta) in &entries {
        h.update(image_id.as_bytes());
        h.update([0u8]);
        h.update(etag_or_fallback(meta).as_bytes());
        h.update([0u8]);
        h.update(meta.size.to_le_bytes());
        if let Some(lm) = meta.last_modified {
            if let Ok(d) = lm.duration_since(UNIX_EPOCH) {
                h.update(d.as_nanos().to_le_bytes());
            }
        }
        h.update([0u8]);
    }

    let digest = h.finalize();
    let etag = format!("\"images-{}\"", hex::encode(&digest[..16]));
    HeaderValue::from_str(&etag).expect("etag must be a valid header value")
}

/// Evaluates conditional request headers for `GET`/`HEAD`.
///
/// Precedence is per RFC 9110:
/// - If `If-None-Match` is present it dominates `If-Modified-Since`.
pub fn is_not_modified(
    req_headers: &HeaderMap,
    current_etag: Option<&str>,
    current_last_modified: Option<SystemTime>,
) -> bool {
    if let Some(inm) = req_headers.get(header::IF_NONE_MATCH) {
        let Some(current_etag) = current_etag else {
            return false;
        };
        return if_none_match_matches(inm, current_etag);
    }

    let Some(ims) = req_headers.get(header::IF_MODIFIED_SINCE) else {
        return false;
    };
    let Some(resource_last_modified) = current_last_modified else {
        return false;
    };
    let Ok(ims) = ims.to_str() else {
        return false;
    };
    let Ok(ims_time) = httpdate::parse_http_date(ims) else {
        return false;
    };

    resource_last_modified <= ims_time
}

fn if_none_match_matches(if_none_match: &HeaderValue, current_etag: &str) -> bool {
    let Ok(if_none_match) = if_none_match.to_str() else {
        return false;
    };

    let current = strip_weak_prefix(current_etag.trim());

    for raw in if_none_match.split(',') {
        let tag = raw.trim();
        if tag == "*" {
            return true;
        }
        let candidate = strip_weak_prefix(tag);
        if candidate == current {
            return true;
        }
    }

    false
}

fn strip_weak_prefix(tag: &str) -> &str {
    tag.trim().strip_prefix("W/").unwrap_or(tag.trim())
}

/// Returns `true` if a request with `Range` may be served as partial content.
///
/// If `If-Range` is absent, this returns `true`.
pub fn if_range_allows_range(
    req_headers: &HeaderMap,
    current_etag: Option<&str>,
    current_last_modified: Option<SystemTime>,
) -> bool {
    let Some(if_range) = req_headers.get(header::IF_RANGE) else {
        return true;
    };
    let Ok(if_range) = if_range.to_str() else {
        return false;
    };
    let if_range = if_range.trim();

    // Entity-tag form. RFC 9110 requires strong comparison and disallows weak validators.
    if if_range.starts_with('"') || if_range.starts_with("W/") {
        let Some(current_etag) = current_etag else {
            return false;
        };
        // If either side is weak, treat it as not matching for If-Range purposes.
        if if_range.starts_with("W/") || current_etag.trim_start().starts_with("W/") {
            return false;
        }
        return if_range == current_etag;
    }

    // HTTP-date form.
    let Ok(since) = httpdate::parse_http_date(if_range) else {
        return false;
    };
    let Some(last_modified) = current_last_modified else {
        return false;
    };
    last_modified <= since
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn weak_etag_is_stable_and_quoted() {
        let modified = UNIX_EPOCH + Duration::from_secs(123) + Duration::from_nanos(456);
        let e1 = weak_etag_from_size_and_mtime(1234, Some(modified));
        let e2 = weak_etag_from_size_and_mtime(1234, Some(modified));

        assert_eq!(e1, e2);
        assert!(e1.starts_with("W/\"") && e1.ends_with('\"'));
    }
}
