use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{header, HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

use crate::store::ImageMeta;

/// Returns `true` if this looks like a syntactically valid HTTP `ETag` header value.
///
/// This is intentionally stricter than `HeaderValue` parsing: we require quoted tags, since an
/// unquoted value is not a valid entity-tag per RFC 9110.
fn looks_like_etag(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if value.len() > crate::store::MAX_ETAG_LEN {
        return false;
    }

    // We compare/emit ETags as strings (e.g. for `If-None-Match` parsing and JSON metadata), so
    // require ASCII. This also avoids subtle behavior differences where `HeaderValue` accepts
    // `obs-text` bytes but `HeaderValue::to_str()` rejects them.
    if !value.is_ascii() {
        return false;
    }

    // Reject any whitespace inside the ETag itself (OWS around the value is handled by `trim()`).
    //
    // This also rejects newlines, which prevents header injection and avoids panics when
    // converting to `HeaderValue`.
    if value.chars().any(|c| c.is_whitespace()) {
        return false;
    }

    let tag = if value.starts_with("W/") || value.starts_with("w/") {
        &value[2..]
    } else {
        value
    };

    if !(tag.len() >= 2 && tag.starts_with('"') && tag.ends_with('"')) {
        return false;
    }

    // RFC 9110 forbids `"` inside the opaque-tag; any quote would prematurely terminate the tag.
    !tag[1..tag.len() - 1].contains('"')
}

/// Build a safe `ETag` header value.
///
/// Store backends can theoretically return arbitrary strings for `ImageMeta::etag`; converting
/// them to `HeaderValue` using `.unwrap()` can panic the server if the value contains invalid
/// header characters (e.g. newlines).
///
/// This helper validates the store-provided tag and falls back to a deterministic safe tag.
pub fn etag_header_value_or_fallback<F>(etag: Option<&str>, fallback: F) -> HeaderValue
where
    F: FnOnce() -> String,
{
    if let Some(etag) = etag {
        // If the raw value is far beyond our max length, treat it as invalid without trimming.
        // This avoids spending O(n) time trimming attacker-controlled whitespace from very large
        // strings.
        if etag.len() > crate::store::MAX_ETAG_LEN + 32 {
            let etag_for_log = super::observability::truncate_for_span(etag, 256);
            tracing::warn!(
                etag = ?etag_for_log.as_ref(),
                len = etag.len(),
                max_len = crate::store::MAX_ETAG_LEN,
                "store-provided ETag is too long; using fallback"
            );
        } else {
            let trimmed = etag.trim();
            if !trimmed.is_ascii() {
                let etag_for_log = super::observability::truncate_for_span(trimmed, 256);
                tracing::warn!(
                    etag = ?etag_for_log.as_ref(),
                    "store-provided ETag is not ASCII; using fallback"
                );
            } else if looks_like_etag(trimmed) {
                match HeaderValue::from_str(trimmed) {
                    Ok(v) => {
                        // `HeaderValue::from_str` allows obs-text, but our conditional request logic
                        // (`If-None-Match` / `If-Range`) parses request headers using `to_str()`, which
                        // rejects non-visible ASCII. Ensure the value we emit is representable as a
                        // header string so clients can round-trip it back to us for cache revalidation.
                        if v.to_str().is_ok() {
                            return v;
                        }

                        let etag_for_log = super::observability::truncate_for_span(trimmed, 256);
                        tracing::warn!(
                            etag = ?etag_for_log.as_ref(),
                            "store-provided ETag contains non-visible ASCII; using fallback"
                        );
                    }
                    Err(err) => {
                        let etag_for_log = super::observability::truncate_for_span(trimmed, 256);
                        tracing::warn!(
                            etag = ?etag_for_log.as_ref(),
                            error = %err,
                            "invalid store-provided ETag header value; using fallback"
                        );
                    }
                }
            } else if !trimmed.is_empty() {
                let etag_for_log = super::observability::truncate_for_span(trimmed, 256);
                tracing::warn!(
                    etag = ?etag_for_log.as_ref(),
                    "store-provided ETag is not a valid HTTP entity-tag; using fallback"
                );
            }
        }
    }

    // Deterministic, safe fallback (weak ETag).
    let fallback = fallback();
    match HeaderValue::from_str(&fallback) {
        Ok(v) => v,
        Err(err) => {
            // This should never happen (we control the fallback format), but keep it panic-free.
            tracing::warn!(error = %err, "generated fallback ETag was not a valid header value");
            HeaderValue::from_static("W/\"0-0-0\"")
        }
    }
}

pub fn etag_header_value_for_meta(meta: &ImageMeta) -> HeaderValue {
    etag_header_value_or_fallback(meta.etag.as_deref(), || {
        weak_etag_from_size_and_mtime(meta.size, meta.last_modified)
    })
}

pub fn etag_or_fallback(meta: &ImageMeta) -> String {
    // Keep this consistent with the value we would send in `ETag` response headers.
    let value = etag_header_value_for_meta(meta);
    match value.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => weak_etag_from_size_and_mtime(meta.size, meta.last_modified),
    }
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
    // `httpdate::fmt_http_date` panics if the time is before the Unix epoch.
    //
    // While pre-epoch mtimes are rare in practice, they can happen (filesystem metadata, or
    // operator-specified values). Avoid crashing the server; omit the header instead.
    if last_modified.duration_since(UNIX_EPOCH).is_err() {
        return None;
    }
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

    // HTTP dates have 1-second resolution. Filesystems often provide sub-second mtimes, but our
    // `Last-Modified` header (and thus `If-Modified-Since`) cannot represent that. Compare at
    // second granularity to avoid false negatives where the resource's mtime has sub-second data
    // that gets truncated when formatting/parsing the HTTP date.
    let Ok(resource_secs) = resource_last_modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let Ok(ims_secs) = ims_time.duration_since(UNIX_EPOCH) else {
        return false;
    };
    resource_secs.as_secs() <= ims_secs.as_secs()
}

fn if_none_match_matches(if_none_match: &HeaderValue, current_etag: &str) -> bool {
    let Ok(if_none_match) = if_none_match.to_str() else {
        return false;
    };

    let current = strip_weak_prefix(current_etag.trim());

    // `If-None-Match` is a comma-separated list of entity-tags, but commas are allowed inside
    // a quoted entity-tag value. Split only on commas that occur *outside* quotes.
    let mut start = 0usize;
    let mut in_quotes = false;
    let bytes = if_none_match.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                let tag = if_none_match[start..i].trim();
                if tag == "*" {
                    return true;
                }
                let candidate = strip_weak_prefix(tag);
                if candidate == current {
                    return true;
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tag = if_none_match[start..].trim();
    if tag == "*" {
        return true;
    }
    let candidate = strip_weak_prefix(tag);
    if candidate == current {
        return true;
    }

    false
}

fn strip_weak_prefix(tag: &str) -> &str {
    let trimmed = tag.trim();
    trimmed
        .strip_prefix("W/")
        .or_else(|| trimmed.strip_prefix("w/"))
        .unwrap_or(trimmed)
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
    if if_range.starts_with('"') || if_range.starts_with("W/") || if_range.starts_with("w/") {
        let Some(current_etag) = current_etag else {
            return false;
        };
        // If either side is weak, treat it as not matching for If-Range purposes.
        let current_etag = current_etag.trim_start();
        if if_range.starts_with("W/")
            || if_range.starts_with("w/")
            || current_etag.starts_with("W/")
            || current_etag.starts_with("w/")
        {
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
    // HTTP dates have 1-second resolution. Filesystems often provide sub-second mtimes, but our
    // `Last-Modified` header (and thus `If-Range` in HTTP-date form) cannot represent that.
    // Compare at second granularity to avoid false mismatches where the resource mtime has
    // sub-second data that gets truncated when formatting/parsing the HTTP date.
    let Ok(resource_secs) = last_modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let Ok(since_secs) = since.duration_since(UNIX_EPOCH) else {
        return false;
    };
    resource_secs.as_secs() <= since_secs.as_secs()
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

    #[test]
    fn if_modified_since_ignores_subsecond_precision() {
        let mut headers = HeaderMap::new();
        let last_modified = UNIX_EPOCH + Duration::from_secs(123) + Duration::from_nanos(456);
        let header_value = httpdate::fmt_http_date(last_modified);
        headers.insert(
            header::IF_MODIFIED_SINCE,
            HeaderValue::from_str(&header_value).unwrap(),
        );

        assert!(
            is_not_modified(&headers, None, Some(last_modified)),
            "expected If-Modified-Since to match even when the resource mtime has sub-second precision"
        );
    }

    #[test]
    fn last_modified_header_value_does_not_panic_for_pre_epoch_times() {
        let t = UNIX_EPOCH - Duration::from_secs(1);
        assert!(last_modified_header_value(Some(t)).is_none());
    }

    #[test]
    fn if_range_http_date_ignores_subsecond_precision() {
        let last_modified = UNIX_EPOCH + Duration::from_secs(123) + Duration::from_nanos(456);
        let if_range_value = httpdate::fmt_http_date(last_modified);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_RANGE,
            HeaderValue::from_str(&if_range_value).unwrap(),
        );

        assert!(
            if_range_allows_range(&headers, None, Some(last_modified)),
            "expected If-Range date to match even when the resource mtime has sub-second precision"
        );
    }

    #[test]
    fn etag_header_value_or_fallback_rejects_inner_quotes() {
        let v = etag_header_value_or_fallback(Some("\"a\"b\""), || "W/\"fallback\"".to_string());
        assert_eq!(v.to_str().unwrap(), "W/\"fallback\"");
    }

    #[test]
    fn etag_header_value_or_fallback_rejects_non_ascii() {
        let v = etag_header_value_or_fallback(Some("\"é\""), || "W/\"fallback\"".to_string());
        assert_eq!(v.to_str().unwrap(), "W/\"fallback\"");
    }

    #[test]
    fn if_none_match_handles_commas_inside_etag() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str("\"a,b\"").unwrap(),
        );

        assert!(
            is_not_modified(&headers, Some("\"a,b\""), None),
            "expected comma inside quoted ETag to be treated as part of the tag"
        );
    }

    #[test]
    fn if_none_match_handles_commas_inside_etag_in_list() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str("W/\"x\", \"a,b\"").unwrap(),
        );

        assert!(is_not_modified(&headers, Some("\"a,b\""), None));
    }
}
