use axum::http::{
    header::{self, HeaderValue},
    HeaderMap,
};
use std::collections::HashSet;

/// Append `Vary` tokens without overwriting any existing `Vary` values.
///
/// - Existing `Vary` values are parsed as CSV tokens.
/// - Tokens are compared case-insensitively (header field-name rules).
/// - The resulting value is written back as a single normalized `Vary` header.
pub(crate) fn append_vary(headers: &mut HeaderMap, tokens: &[&str]) {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Per RFC 9110, `Vary: *` means "vary on everything" and makes the response effectively
    // uncacheable by shared caches. If it exists, preserve it verbatim and don't try to append.
    let mut has_star = false;

    for value in headers.get_all(header::VARY).iter() {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for raw in value.split(',') {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }
            if token == "*" {
                has_star = true;
                break;
            }
            let key = token.to_ascii_lowercase();
            if seen.insert(key) {
                out.push(token.to_string());
            }
        }
        if has_star {
            break;
        }
    }

    if has_star {
        headers.remove(header::VARY);
        headers.insert(header::VARY, HeaderValue::from_static("*"));
        return;
    }

    for token in tokens {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token == "*" {
            headers.remove(header::VARY);
            headers.insert(header::VARY, HeaderValue::from_static("*"));
            return;
        }
        let key = token.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(token.to_string());
        }
    }

    if out.is_empty() {
        return;
    }

    let normalized = out.join(", ");
    if let Ok(value) = HeaderValue::from_str(&normalized) {
        headers.remove(header::VARY);
        headers.insert(header::VARY, value);
    }
}

