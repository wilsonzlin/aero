use aero_auth_tokens::{verify_gateway_session_token_checked, verify_hs256_jwt_checked};
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
    exp: Option<i64>,
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
    now_sec: i64,
    sid: Option<String>,
    exp: Option<i64>,
    iat: Option<i64>,
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
        if v.expect_error {
            let err = verify_gateway_session_token_checked(&v.token, v.secret.as_bytes(), v.now_ms)
                .expect_err("expected error");
            let _ = err;
            continue;
        }

        let got = verify_gateway_session_token_checked(&v.token, v.secret.as_bytes(), v.now_ms)
            .unwrap_or_else(|err| panic!("expected ok for {}: {err}", v.name));
        assert_eq!(got.sid, v.sid.expect("sid"));
        assert_eq!(got.exp_unix, v.exp.expect("exp"));
    }
}

#[test]
fn protocol_jwt_vectors() {
    let vf: VectorFile = serde_json::from_str(VECTORS_JSON).expect("parse auth-tokens.json");
    assert_eq!(vf.schema, 1);

    for v in vf.jwt_tokens.vectors {
        if v.expect_error {
            let err = verify_hs256_jwt_checked(&v.token, v.secret.as_bytes(), v.now_sec)
                .expect_err("expected error");
            let _ = err;
            continue;
        }

        let got = verify_hs256_jwt_checked(&v.token, v.secret.as_bytes(), v.now_sec)
            .unwrap_or_else(|err| panic!("expected ok for {}: {err}", v.name));
        assert_eq!(got.sid, v.sid.expect("sid"));
        assert_eq!(got.exp, v.exp.expect("exp"));
        assert_eq!(got.iat, v.iat.expect("iat"));
        assert_eq!(got.origin, v.origin);
        assert_eq!(got.aud, v.aud);
        assert_eq!(got.iss, v.iss);
    }
}
