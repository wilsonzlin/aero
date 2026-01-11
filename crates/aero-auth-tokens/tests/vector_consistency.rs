use serde::Deserialize;

const CONFORMANCE_VECTORS_JSON: &str =
    include_str!("../../conformance/test-vectors/aero-vectors-v1.json");
const PROTOCOL_VECTORS_JSON: &str = include_str!("../../../protocol-vectors/auth-tokens.json");

#[derive(Debug, Deserialize)]
struct ConformanceRoot {
    version: u32,
    aero_session: ConformanceSessionVectors,
    #[serde(rename = "aero-udp-relay-jwt-hs256")]
    relay_jwt: ConformanceJwtVectors,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionVectors {
    secret: String,
    #[serde(rename = "nowMs")]
    now_ms: u64,
    tokens: ConformanceSessionTokenSet,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionTokenSet {
    valid: ConformanceSessionTokenVector,
    expired: ConformanceSessionTokenVector,
    #[serde(rename = "badSignature")]
    bad_signature: ConformanceSessionTokenVector,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionTokenVector {
    token: String,
    claims: ConformanceSessionClaims,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionClaims {
    sid: String,
    exp: i64,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtVectors {
    secret: String,
    #[serde(rename = "nowUnix")]
    now_unix: i64,
    tokens: ConformanceJwtTokenSet,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtTokenSet {
    valid: ConformanceJwtTokenVector,
    expired: ConformanceJwtTokenVector,
    #[serde(rename = "badSignature")]
    bad_signature: ConformanceJwtTokenVector,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtTokenVector {
    token: String,
    claims: ConformanceJwtClaims,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtClaims {
    sid: String,
    exp: i64,
    iat: i64,
    origin: String,
    aud: String,
    iss: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolRoot {
    schema: u32,
    #[serde(rename = "sessionTokens")]
    session_tokens: ProtocolSessionSection,
    #[serde(rename = "jwtTokens")]
    jwt_tokens: ProtocolJwtSection,
}

#[derive(Debug, Deserialize)]
struct ProtocolSessionSection {
    #[serde(rename = "testSecret")]
    test_secret: String,
    vectors: Vec<ProtocolSessionVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolSessionVector {
    name: String,
    secret: String,
    token: String,
    #[serde(rename = "nowMs")]
    now_ms: u64,
    sid: Option<String>,
    exp: Option<i64>,
    #[serde(rename = "expectError", default)]
    _expect_error: bool,
}

#[derive(Debug, Deserialize)]
struct ProtocolJwtSection {
    #[serde(rename = "testSecret")]
    test_secret: String,
    vectors: Vec<ProtocolJwtVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolJwtVector {
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
    _expect_error: bool,
}

fn find_protocol_session<'a>(
    vectors: &'a [ProtocolSessionVector],
    name: &str,
) -> &'a ProtocolSessionVector {
    vectors
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("missing protocol session vector {name}"))
}

fn find_protocol_jwt<'a>(vectors: &'a [ProtocolJwtVector], name: &str) -> &'a ProtocolJwtVector {
    vectors
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("missing protocol jwt vector {name}"))
}

#[test]
fn protocol_vectors_match_conformance_vectors_for_canonical_tokens() {
    let conformance: ConformanceRoot =
        serde_json::from_str(CONFORMANCE_VECTORS_JSON).expect("parse conformance vectors json");
    assert_eq!(
        conformance.version, 1,
        "unexpected conformance vector version"
    );

    let protocol: ProtocolRoot =
        serde_json::from_str(PROTOCOL_VECTORS_JSON).expect("parse protocol vectors json");
    assert_eq!(protocol.schema, 1, "unexpected protocol vector schema");

    assert_eq!(
        protocol.session_tokens.test_secret, conformance.aero_session.secret,
        "session test secret mismatch"
    );
    assert_eq!(
        protocol.jwt_tokens.test_secret, conformance.relay_jwt.secret,
        "jwt test secret mismatch"
    );

    // Session tokens (gateway cookie)
    let p_valid = find_protocol_session(&protocol.session_tokens.vectors, "valid");
    assert_eq!(p_valid.token, conformance.aero_session.tokens.valid.token);
    assert_eq!(p_valid.secret, conformance.aero_session.secret);
    assert_eq!(
        p_valid.sid.as_deref(),
        Some(conformance.aero_session.tokens.valid.claims.sid.as_str())
    );
    assert_eq!(
        p_valid.exp,
        Some(conformance.aero_session.tokens.valid.claims.exp)
    );
    assert_eq!(p_valid.now_ms, conformance.aero_session.now_ms);

    let p_expired = find_protocol_session(&protocol.session_tokens.vectors, "expired");
    assert_eq!(
        p_expired.token,
        conformance.aero_session.tokens.expired.token
    );

    let p_bad_sig = find_protocol_session(&protocol.session_tokens.vectors, "badSignature");
    assert_eq!(
        p_bad_sig.token,
        conformance.aero_session.tokens.bad_signature.token
    );

    // Relay JWTs
    let j_valid = find_protocol_jwt(&protocol.jwt_tokens.vectors, "valid");
    assert_eq!(j_valid.token, conformance.relay_jwt.tokens.valid.token);
    assert_eq!(j_valid.secret, conformance.relay_jwt.secret);
    assert_eq!(
        j_valid.sid.as_deref(),
        Some(conformance.relay_jwt.tokens.valid.claims.sid.as_str())
    );
    assert_eq!(
        j_valid.exp,
        Some(conformance.relay_jwt.tokens.valid.claims.exp)
    );
    assert_eq!(
        j_valid.iat,
        Some(conformance.relay_jwt.tokens.valid.claims.iat)
    );
    assert_eq!(
        j_valid.origin.as_deref(),
        Some(conformance.relay_jwt.tokens.valid.claims.origin.as_str())
    );
    assert_eq!(
        j_valid.aud.as_deref(),
        Some(conformance.relay_jwt.tokens.valid.claims.aud.as_str())
    );
    assert_eq!(
        j_valid.iss.as_deref(),
        Some(conformance.relay_jwt.tokens.valid.claims.iss.as_str())
    );
    assert_eq!(j_valid.now_sec, conformance.relay_jwt.now_unix);

    let j_expired = find_protocol_jwt(&protocol.jwt_tokens.vectors, "expired");
    assert_eq!(j_expired.token, conformance.relay_jwt.tokens.expired.token);

    let j_bad_sig = find_protocol_jwt(&protocol.jwt_tokens.vectors, "badSignature");
    assert_eq!(
        j_bad_sig.token,
        conformance.relay_jwt.tokens.bad_signature.token
    );
}
