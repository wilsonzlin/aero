use url::Url;

pub(crate) fn normalize_origin(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "null" {
        return Some("null".to_string());
    }

    let url = Url::parse(trimmed).ok()?;

    let scheme = url.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }

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
        url::Host::Domain(domain) => domain.to_ascii_lowercase(),
        url::Host::Ipv4(addr) => addr.to_string(),
        url::Host::Ipv6(addr) => format!("[{addr}]"),
    };

    let mut port = url.port();
    if matches!((&*scheme, port), ("http", Some(80)) | ("https", Some(443))) {
        port = None;
    }

    Some(match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

#[cfg(test)]
mod tests {
    use super::normalize_origin;
    use serde::Deserialize;
    use std::{fs, path::PathBuf};

    #[derive(Debug, Deserialize)]
    struct Vector {
        raw: String,
        normalized: Option<String>,
    }

    #[test]
    fn matches_shared_vectors() {
        let vectors_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/origin-allowlist-test-vectors.json");
        let contents = fs::read_to_string(&vectors_path)
            .unwrap_or_else(|err| panic!("read {}: {err}", vectors_path.display()));
        let vectors: Vec<Vector> =
            serde_json::from_str(&contents).expect("parse origin-allowlist-test-vectors.json");

        for vector in vectors {
            let got = normalize_origin(&vector.raw);
            assert_eq!(got, vector.normalized, "raw={}", vector.raw);
        }
    }
}

