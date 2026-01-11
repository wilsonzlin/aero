use aero_auth_tokens::{verify_gateway_session_token, verify_hs256_jwt};
use serde::Deserialize;

const VECTORS_JSON: &str = include_str!("../../conformance/test-vectors/aero-vectors-v1.json");

#[derive(Debug, Deserialize)]
struct RootVectors {
    version: u32,
    aero_session: SessionVectors,
    #[serde(rename = "aero-udp-relay-jwt-hs256")]
    udp_relay_jwt: JwtVectors,
}

#[derive(Debug, Deserialize)]
struct SessionVectors {
    secret: String,
    #[serde(rename = "nowMs")]
    now_ms: u64,
    tokens: SessionTokenSet,
}

#[derive(Debug, Deserialize)]
struct SessionTokenSet {
    valid: SessionTokenVector,
    expired: SessionTokenVector,
    #[serde(rename = "badSignature")]
    bad_signature: SessionTokenVector,
}

#[derive(Debug, Deserialize)]
struct SessionTokenVector {
    token: String,
    claims: SessionClaims,
}

#[derive(Debug, Deserialize)]
struct SessionClaims {
    sid: String,
    exp: i64,
}

#[derive(Debug, Deserialize)]
struct JwtVectors {
    secret: String,
    #[serde(rename = "nowUnix")]
    now_unix: i64,
    tokens: JwtTokenSet,
}

#[derive(Debug, Deserialize)]
struct JwtTokenSet {
    valid: JwtTokenVector,
    expired: JwtTokenVector,
    #[serde(rename = "badSignature")]
    bad_signature: JwtTokenVector,
}

#[derive(Debug, Deserialize)]
struct JwtTokenVector {
    token: String,
    claims: JwtClaims,
}

#[derive(Debug, Deserialize)]
struct JwtClaims {
    sid: String,
    exp: i64,
    iat: i64,
    origin: Option<String>,
    aud: Option<String>,
    iss: Option<String>,
}

#[test]
fn session_token_vectors() {
    let vf: RootVectors = serde_json::from_str(VECTORS_JSON).expect("parse vectors json");
    assert_eq!(vf.version, 1, "unexpected vector file version");

    let secret = vf.aero_session.secret.as_bytes();
    let now_ms = vf.aero_session.now_ms;

    let valid = verify_gateway_session_token(&vf.aero_session.tokens.valid.token, secret, now_ms)
        .expect("expected valid session token to verify");
    assert_eq!(valid.sid, vf.aero_session.tokens.valid.claims.sid);
    assert_eq!(valid.exp_unix, vf.aero_session.tokens.valid.claims.exp);

    assert!(
        verify_gateway_session_token(&vf.aero_session.tokens.expired.token, secret, now_ms).is_none(),
        "expected expired session token to be rejected"
    );
    assert!(
        verify_gateway_session_token(&vf.aero_session.tokens.bad_signature.token, secret, now_ms).is_none(),
        "expected bad signature session token to be rejected"
    );
}

#[test]
fn jwt_vectors() {
    let vf: RootVectors = serde_json::from_str(VECTORS_JSON).expect("parse vectors json");
    assert_eq!(vf.version, 1, "unexpected vector file version");

    let secret = vf.udp_relay_jwt.secret.as_bytes();
    let now = vf.udp_relay_jwt.now_unix;

    let got = verify_hs256_jwt(&vf.udp_relay_jwt.tokens.valid.token, secret, now)
        .expect("expected valid jwt to verify");
    assert_eq!(got.sid, vf.udp_relay_jwt.tokens.valid.claims.sid);
    assert_eq!(got.exp, vf.udp_relay_jwt.tokens.valid.claims.exp);
    assert_eq!(got.iat, vf.udp_relay_jwt.tokens.valid.claims.iat);
    assert_eq!(got.origin, vf.udp_relay_jwt.tokens.valid.claims.origin);
    assert_eq!(got.aud, vf.udp_relay_jwt.tokens.valid.claims.aud);
    assert_eq!(got.iss, vf.udp_relay_jwt.tokens.valid.claims.iss);

    assert!(
        verify_hs256_jwt(&vf.udp_relay_jwt.tokens.expired.token, secret, now).is_none(),
        "expected expired jwt to be rejected"
    );
    assert!(
        verify_hs256_jwt(&vf.udp_relay_jwt.tokens.bad_signature.token, secret, now).is_none(),
        "expected bad signature jwt to be rejected"
    );
}
