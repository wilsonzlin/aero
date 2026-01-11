#![cfg(not(target_arch = "wasm32"))]

use std::{fs, path::PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct OriginVectorsFile {
    schema: u32,
    normalize: Vec<NormalizeVector>,
    allow: Vec<AllowVector>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NormalizeVector {
    name: String,
    raw_origin_header: String,
    normalized_origin: Option<String>,
    #[serde(default)]
    expect_error: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AllowVector {
    name: String,
    allowed_origins: Vec<String>,
    request_host: String,
    raw_origin_header: String,
    expect_allowed: bool,
}

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol-vectors/origin.json")
}

#[test]
fn origin_vectors_conformance() {
    let raw = fs::read_to_string(vectors_path()).expect("read protocol-vectors/origin.json");
    let vectors: OriginVectorsFile = serde_json::from_str(&raw).expect("parse origin.json");
    assert_eq!(vectors.schema, 1, "unexpected origin.json schema");

    for v in vectors.normalize {
        let got = aero_l2_proxy::origin::normalize_origin(&v.raw_origin_header);
        if v.expect_error {
            assert!(
                got.is_none(),
                "normalize/{}: expected error, got ok ({got:?})",
                v.name
            );
            continue;
        }

        let expected = v
            .normalized_origin
            .expect("normalize vectors without expectError must include normalizedOrigin");
        assert_eq!(got, Some(expected), "normalize/{} mismatch", v.name);
    }

    for v in vectors.allow {
        let got = aero_l2_proxy::origin::is_origin_allowed(
            &v.raw_origin_header,
            &v.request_host,
            &v.allowed_origins,
        );
        assert_eq!(got, v.expect_allowed, "allow/{} mismatch", v.name);
    }
}
