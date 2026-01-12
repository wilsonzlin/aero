use url::{Host, Url};

fn authority_has_userinfo(raw: &str) -> bool {
    let trimmed = raw.trim();
    let Some(scheme_sep) = trimmed.find("://") else {
        return false;
    };
    let start = scheme_sep + 3;
    let end = trimmed[start..]
        .find(&['/', '?', '#'][..])
        .map(|i| start + i)
        .unwrap_or(trimmed.len());
    trimmed[start..end].contains('@')
}

fn scheme_from_normalized_origin(normalized_origin: &str) -> Option<&'static str> {
    if normalized_origin.starts_with("http://") {
        Some("http")
    } else if normalized_origin.starts_with("https://") {
        Some("https")
    } else {
        None
    }
}

fn host_from_normalized_origin(normalized_origin: &str) -> Option<&str> {
    let (_, host) = normalized_origin.split_once("://")?;
    Some(host)
}

fn serialize_ipv6(addr: &std::net::Ipv6Addr) -> String {
    let segments = addr.segments();

    let mut best_start: Option<usize> = None;
    let mut best_len = 0;
    let mut cur_start: Option<usize> = None;
    let mut cur_len = 0;

    for (i, seg) in segments.iter().enumerate() {
        if *seg == 0 {
            if cur_start.is_none() {
                cur_start = Some(i);
                cur_len = 1;
            } else {
                cur_len += 1;
            }
            continue;
        }

        if cur_len >= 2 && cur_len > best_len {
            best_start = cur_start;
            best_len = cur_len;
        }
        cur_start = None;
        cur_len = 0;
    }

    if cur_len >= 2 && cur_len > best_len {
        best_start = cur_start;
        best_len = cur_len;
    }

    let mut out = String::new();
    let mut need_sep = false;
    let mut i = 0;
    while i < segments.len() {
        if best_start == Some(i) {
            out.push_str("::");
            need_sep = false;
            i += best_len;
            if i >= segments.len() {
                break;
            }
            continue;
        }

        if need_sep {
            out.push(':');
        }
        out.push_str(&format!("{:x}", segments[i]));
        need_sep = true;
        i += 1;
    }

    out
}

fn normalize_request_host(request_host: &str, scheme: &'static str) -> Option<String> {
    let trimmed = request_host.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Host is an ASCII serialization. Be strict about rejecting non-ASCII or
    // non-printable characters that URL parsers may normalize away.
    if !trimmed.chars().all(|c| c.is_ascii_graphic()) {
        return None;
    }
    // Reject percent-encoding / IPv6 zone identifiers to avoid cross-language
    // parsing differences.
    if trimmed.contains('%') {
        return None;
    }
    // Reject comma-delimited values. Some HTTP stacks join repeated headers with commas.
    if trimmed.contains(',') {
        return None;
    }
    // Host is a host[:port] serialization; reject any path/query/fragment delimiters.
    if trimmed.contains('/') || trimmed.contains('?') || trimmed.contains('#') {
        return None;
    }
    // Reject backslashes; some URL parsers normalize them to `/`.
    if trimmed.contains('\\') {
        return None;
    }
    // Reject any userinfo in the request host.
    if trimmed.contains('@') {
        return None;
    }
    // Reject empty port specs like `example.com:`. (The `:/` case is handled
    // indirectly by URL parsing when we add the scheme prefix.)
    if trimmed.ends_with(':') {
        return None;
    }

    let url = Url::parse(&format!("{scheme}://{trimmed}")).ok()?;

    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    if url.path() != "/" && !url.path().is_empty() {
        return None;
    }

    let host = match url.host()? {
        Host::Domain(domain) => domain.to_ascii_lowercase(),
        Host::Ipv4(addr) => addr.to_string(),
        Host::Ipv6(addr) => format!("[{}]", serialize_ipv6(&addr)),
    };

    let mut port = url.port();
    if port == Some(0) {
        return None;
    }
    if matches!((scheme, port), ("http", Some(80)) | ("https", Some(443))) {
        port = None;
    }

    Some(match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// Normalize an Origin header string.
///
/// This matches the canonical vectors in `protocol-vectors/origin.json`.
pub fn normalize_origin(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "null" {
        return Some("null".to_string());
    }

    // Origin is an ASCII serialization (RFC 6454 / WHATWG URL). Be strict about
    // rejecting non-ASCII or non-printable characters that URL parsers may
    // normalize away.
    if !trimmed.chars().all(|c| c.is_ascii_graphic()) {
        return None;
    }
    // Reject percent-encoding and IPv6 zone identifiers; browsers don't emit
    // these in Origin, and different URL libraries disagree on how to handle them.
    if trimmed.contains('%') {
        return None;
    }
    // Reject comma-delimited values. Browsers send a single Origin serialization,
    // but some HTTP stacks may join repeated headers with commas.
    if trimmed.contains(',') {
        return None;
    }
    // Some URL libraries (notably Go's net/url) reject additional host codepoints
    // that WHATWG URL parsers accept. Reject them here so Origin validation stays
    // consistent across components.
    if trimmed.contains('{') || trimmed.contains('}') || trimmed.contains('`') {
        return None;
    }
    // Require an explicit scheme://host serialization; `url` will happily
    // normalize `https:example.com` to `https://example.com/`, but browsers won't
    // emit those in Origin headers.
    let (scheme_len, rest) = if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("http://")
    {
        (7usize, &trimmed[7..])
    } else if trimmed.len() >= 8 && trimmed[..8].eq_ignore_ascii_case("https://") {
        (8usize, &trimmed[8..])
    } else {
        return None;
    };
    if rest.starts_with('/') {
        return None;
    }
    // Allow an optional trailing slash, but reject any other path segments.
    // The `url` crate normalizes dot segments (e.g. "/." or "/..") to "/",
    // which would otherwise cause non-origin strings to be accepted.
    if let Some(pos) = trimmed[scheme_len..].find('/') {
        if scheme_len + pos != trimmed.len() - 1 {
            return None;
        }
    }
    // Reject backslashes; some URL parsers normalize them to `/`, which can
    // silently change the host/path boundary.
    if trimmed.contains('\\') {
        return None;
    }
    // Reject empty port specs like `https://example.com:` or `https://example.com:/`.
    if trimmed.ends_with(':') || trimmed.ends_with(":/") {
        return None;
    }
    // The URL parser loses information about empty usernames (e.g.
    // https://@example.com), so detect userinfo in the raw authority section.
    if authority_has_userinfo(trimmed) {
        return None;
    }

    let url = Url::parse(trimmed).ok()?;

    let scheme = url.scheme();
    let scheme = if scheme.eq_ignore_ascii_case("http") {
        "http"
    } else if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else {
        return None;
    };

    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    if url.path() != "/" && !url.path().is_empty() {
        return None;
    }

    let host = match url.host()? {
        Host::Domain(domain) => domain.to_ascii_lowercase(),
        Host::Ipv4(addr) => addr.to_string(),
        Host::Ipv6(addr) => format!("[{}]", serialize_ipv6(&addr)),
    };

    let mut port = url.port();
    if port == Some(0) {
        return None;
    }
    if matches!((scheme, port), ("http", Some(80)) | ("https", Some(443))) {
        port = None;
    }

    Some(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// Returns true when the request Origin header is allowed.
///
/// When `allowed_origins` is empty, the default policy is same-host only, based
/// on the request's Host header. Default ports are treated as equivalent.
pub fn is_origin_allowed(
    raw_origin_header: &str,
    request_host: &str,
    allowed_origins: &[String],
) -> bool {
    let normalized = match normalize_origin(raw_origin_header) {
        Some(v) => v,
        None => return false,
    };

    if allowed_origins.iter().any(|v| v == "*") {
        return true;
    }
    if !allowed_origins.is_empty() {
        return allowed_origins.iter().any(|v| v == &normalized);
    }

    let Some(scheme) = scheme_from_normalized_origin(&normalized) else {
        // "null" (or anything unexpected) cannot match a host-based request.
        return false;
    };
    let Some(origin_host) = host_from_normalized_origin(&normalized) else {
        return false;
    };
    let Some(request_host) = normalize_request_host(request_host, scheme) else {
        return false;
    };
    origin_host == request_host
}
