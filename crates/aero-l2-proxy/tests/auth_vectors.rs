#![cfg(not(target_arch = "wasm32"))]

use aero_l2_proxy::auth::{
    mint_relay_jwt_hs256, mint_session_token, verify_relay_jwt_hs256, verify_session_token,
    JwtVerifyError, RelayJwtClaims, SessionClaims,
};
use serde::Deserialize;

const VECTORS_JSON: &str = include_str!("../../conformance/test-vectors/aero-vectors-v1.json");

#[derive(Debug, Deserialize)]
struct RootVectors {
    version: u32,
    aero_session: SessionVectors,
    #[serde(rename = "aero-udp-relay-jwt-hs256")]
    relay_jwt: JwtVectors,
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
    claims: SessionClaimsVector,
}

#[derive(Debug, Deserialize)]
struct SessionClaimsVector {
    sid: String,
    exp: u64,
}

#[derive(Debug, Deserialize)]
struct JwtVectors {
    secret: String,
    #[serde(rename = "nowUnix")]
    now_unix: u64,
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
    claims: RelayJwtClaimsVector,
}

#[derive(Debug, Deserialize)]
struct RelayJwtClaimsVector {
    iat: u64,
    exp: u64,
    sid: String,
    origin: String,
    aud: String,
    iss: String,
}

#[test]
fn session_cookie_vectors() {
    let vectors: RootVectors = serde_json::from_str(VECTORS_JSON).expect("parse vectors json");
    assert_eq!(vectors.version, 1, "unexpected vector file version");

    let secret = vectors.aero_session.secret.as_bytes();
    let now_ms = vectors.aero_session.now_ms;

    let valid_claims = SessionClaims {
        sid: vectors.aero_session.tokens.valid.claims.sid.clone(),
        exp: vectors.aero_session.tokens.valid.claims.exp,
    };
    assert_eq!(
        mint_session_token(&valid_claims, secret).expect("mint session token"),
        vectors.aero_session.tokens.valid.token,
        "mint valid token"
    );
    assert_eq!(
        verify_session_token(&vectors.aero_session.tokens.valid.token, secret, now_ms),
        Some(valid_claims.clone()),
        "verify valid token"
    );

    let expired_claims = SessionClaims {
        sid: vectors.aero_session.tokens.expired.claims.sid.clone(),
        exp: vectors.aero_session.tokens.expired.claims.exp,
    };
    assert_eq!(
        mint_session_token(&expired_claims, secret).expect("mint session token"),
        vectors.aero_session.tokens.expired.token,
        "mint expired token"
    );
    assert!(
        verify_session_token(&vectors.aero_session.tokens.expired.token, secret, now_ms).is_none(),
        "verify expired token"
    );

    assert_ne!(
        mint_session_token(&valid_claims, secret).expect("mint session token"),
        vectors.aero_session.tokens.bad_signature.token,
        "bad signature token must differ from a properly minted token"
    );
    assert!(
        verify_session_token(&vectors.aero_session.tokens.bad_signature.token, secret, now_ms)
            .is_none(),
        "verify bad signature token"
    );
}

#[test]
fn relay_jwt_vectors() {
    let vectors: RootVectors = serde_json::from_str(VECTORS_JSON).expect("parse vectors json");
    assert_eq!(vectors.version, 1, "unexpected vector file version");

    let secret = vectors.relay_jwt.secret.as_bytes();
    let now_unix = vectors.relay_jwt.now_unix;

    let valid_claims = RelayJwtClaims {
        sid: vectors.relay_jwt.tokens.valid.claims.sid.clone(),
        exp: i64::try_from(vectors.relay_jwt.tokens.valid.claims.exp).expect("exp i64"),
        iat: i64::try_from(vectors.relay_jwt.tokens.valid.claims.iat).expect("iat i64"),
        origin: Some(vectors.relay_jwt.tokens.valid.claims.origin.clone()),
        aud: Some(vectors.relay_jwt.tokens.valid.claims.aud.clone()),
        iss: Some(vectors.relay_jwt.tokens.valid.claims.iss.clone()),
    };
    assert_eq!(
        mint_relay_jwt_hs256(&valid_claims, secret).expect("mint relay jwt"),
        vectors.relay_jwt.tokens.valid.token,
        "mint valid jwt"
    );
    assert_eq!(
        verify_relay_jwt_hs256(&vectors.relay_jwt.tokens.valid.token, secret, now_unix),
        Ok(valid_claims.clone()),
        "verify valid jwt"
    );

    let expired_claims = RelayJwtClaims {
        sid: vectors.relay_jwt.tokens.expired.claims.sid.clone(),
        exp: i64::try_from(vectors.relay_jwt.tokens.expired.claims.exp).expect("exp i64"),
        iat: i64::try_from(vectors.relay_jwt.tokens.expired.claims.iat).expect("iat i64"),
        origin: Some(vectors.relay_jwt.tokens.expired.claims.origin.clone()),
        aud: Some(vectors.relay_jwt.tokens.expired.claims.aud.clone()),
        iss: Some(vectors.relay_jwt.tokens.expired.claims.iss.clone()),
    };
    assert_eq!(
        mint_relay_jwt_hs256(&expired_claims, secret).expect("mint relay jwt"),
        vectors.relay_jwt.tokens.expired.token,
        "mint expired jwt"
    );
    assert_eq!(
        verify_relay_jwt_hs256(&vectors.relay_jwt.tokens.expired.token, secret, now_unix),
        Err(JwtVerifyError::Expired),
        "verify expired jwt"
    );

    assert_ne!(
        mint_relay_jwt_hs256(&valid_claims, secret).expect("mint relay jwt"),
        vectors.relay_jwt.tokens.bad_signature.token,
        "bad signature token must differ from a properly minted jwt"
    );
    assert_eq!(
        verify_relay_jwt_hs256(
            &vectors.relay_jwt.tokens.bad_signature.token,
            secret,
            now_unix
        ),
        Err(JwtVerifyError::InvalidSignature),
        "verify bad signature jwt"
    );
}
