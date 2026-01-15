#![cfg(not(target_arch = "wasm32"))]

use aero_l2_proxy::auth::{verify_relay_jwt_hs256, verify_session_token};
use serde::Deserialize;

const VECTORS_JSON: &str = include_str!("../../../protocol-vectors/auth-tokens.json");

#[derive(Debug, Deserialize)]
struct VectorFile {
    schema: u32,
    #[serde(rename = "sessionTokens")]
    session_tokens: SessionTokensSection,
    #[serde(rename = "jwtTokens")]
    jwt_tokens: JwtTokensSection,
}

#[derive(Debug, Deserialize)]
struct SessionTokensSection {
    #[serde(rename = "testSecret")]
    _test_secret: String,
    vectors: Vec<SessionTokenVector>,
}

#[derive(Debug, Deserialize)]
struct SessionTokenVector {
    name: String,
    secret: String,
    token: String,
    #[serde(rename = "nowMs")]
    now_ms: u64,
    sid: Option<String>,
    exp: Option<u64>,
    #[serde(rename = "expectError", default)]
    expect_error: bool,
}

#[derive(Debug, Deserialize)]
struct JwtTokensSection {
    #[serde(rename = "testSecret")]
    _test_secret: String,
    vectors: Vec<JwtVector>,
}

#[derive(Debug, Deserialize)]
struct JwtVector {
    name: String,
    secret: String,
    token: String,
    #[serde(rename = "nowSec")]
    now_sec: u64,
    sid: Option<String>,
    exp: Option<u64>,
    iat: Option<u64>,
    origin: Option<String>,
    aud: Option<String>,
    iss: Option<String>,
    #[serde(rename = "expectError", default)]
    expect_error: bool,
}

#[test]
fn protocol_session_token_vectors() {
    let vf: VectorFile = serde_json::from_str(VECTORS_JSON).expect("parse auth-tokens.json");
    assert_eq!(vf.schema, 1);

    for v in vf.session_tokens.vectors {
        let got = verify_session_token(&v.token, v.secret.as_bytes(), v.now_ms);
        if v.expect_error {
            assert!(got.is_none(), "expected error for {}", v.name);
            continue;
        }

        let got = got.unwrap_or_else(|| panic!("expected ok for {}", v.name));
        assert_eq!(got.sid, v.sid.expect("sid"));
        assert_eq!(got.exp, v.exp.expect("exp"));
    }
}

#[test]
fn protocol_jwt_vectors() {
    let vf: VectorFile = serde_json::from_str(VECTORS_JSON).expect("parse auth-tokens.json");
    assert_eq!(vf.schema, 1);

    for v in vf.jwt_tokens.vectors {
        let got = verify_relay_jwt_hs256(&v.token, v.secret.as_bytes(), v.now_sec);
        if v.expect_error {
            assert!(got.is_err(), "expected error for {}", v.name);
            continue;
        }

        let got = got.unwrap_or_else(|err| panic!("expected ok for {}: {err:?}", v.name));
        assert_eq!(got.sid, v.sid.expect("sid"));
        assert_eq!(
            got.exp,
            i64::try_from(v.exp.expect("exp")).expect("exp i64")
        );
        assert_eq!(
            got.iat,
            i64::try_from(v.iat.expect("iat")).expect("iat i64")
        );
        assert_eq!(got.origin, v.origin);
        assert_eq!(got.aud, v.aud);
        assert_eq!(got.iss, v.iss);
    }
}
