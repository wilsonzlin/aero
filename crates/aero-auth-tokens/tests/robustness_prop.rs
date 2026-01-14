#![cfg(not(target_arch = "wasm32"))]

use aero_auth_tokens::{verify_gateway_session_token, verify_hs256_jwt};
use base64::{engine::general_purpose, Engine as _};
use proptest::prelude::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const SECRET: &[u8] = b"prop-test-secret";
const JWT_HEADER_JSON: &[u8] = br#"{"alg":"HS256","typ":"JWT"}"#;
const JWT_HEADER_NO_TYP_JSON: &[u8] = br#"{"alg":"HS256"}"#;

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    // HMAC-SHA256 as defined in RFC 2104 (duplicated here to mint test vectors without depending
    // on internal `aero-auth-tokens` helpers).
    const BLOCK_LEN: usize = 64;
    let mut key_block = [0u8; BLOCK_LEN];
    if key.len() > BLOCK_LEN {
        let digest = Sha256::digest(key);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0u8; BLOCK_LEN];
    let mut opad = [0u8; BLOCK_LEN];
    for i in 0..BLOCK_LEN {
        ipad[i] = key_block[i] ^ 0x36;
        opad[i] = key_block[i] ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let outer_hash = outer.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&outer_hash);
    out
}

fn mint_session_token(payload: &[u8], secret: &[u8]) -> String {
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{payload_b64}.{sig_b64}")
}

fn mint_hs256_jwt(payload: &[u8], secret: &[u8]) -> String {
    mint_hs256_jwt_with_header(JWT_HEADER_JSON, payload, secret)
}

fn mint_hs256_jwt_with_header(header: &[u8], payload: &[u8], secret: &[u8]) -> String {
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{signing_input}.{sig_b64}")
}

fn ascii_ident(max_len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::char::range('a', 'z'), 1..=max_len)
        .prop_map(|chars| chars.into_iter().collect())
}

fn ascii_segment(max_len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(0u8..=127u8, 0..=max_len)
        .prop_map(|bytes| bytes.into_iter().map(|b| b as char).collect())
}

fn token_strings() -> impl Strategy<Value = String> {
    // Exercise both arbitrary strings and token-shaped inputs with extra dots / long segments.
    prop_oneof![
        10 => ".*",
        2 => proptest::collection::vec(any::<u8>(), 0..=20_000)
            .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        3 => (
            ascii_segment(2_000),
            ascii_segment(2_000),
            ascii_segment(2_000),
            ascii_segment(2_000),
            ascii_segment(2_000),
        )
            .prop_map(|(a, b, c, d, e)| format!("{a}.{b}.{c}.{d}.{e}")),
    ]
}

proptest! {
    // Keep the property tests fast and deterministic enough for CI, while still exploring
    // a wide input space.
    #![proptest_config(ProptestConfig {
        cases: 256,
        rng_algorithm: proptest::test_runner::RngAlgorithm::ChaCha,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0xA3_A0_47),
        .. ProptestConfig::default()
    })]

    #[test]
    fn verify_gateway_session_token_never_panics(token in token_strings()) {
        let now_ms = 1_700_000_000_000u64;
        let res = std::panic::catch_unwind(|| verify_gateway_session_token(&token, SECRET, now_ms));
        prop_assert!(res.is_ok(), "verify_gateway_session_token panicked (len={})", token.len());

        if let Some(verified) = res.unwrap() {
            prop_assert!(!verified.sid.is_empty());
        }
    }

    #[test]
    fn verify_hs256_jwt_never_panics(token in token_strings()) {
        let now_sec = 1_700_000_000i64;
        let res = std::panic::catch_unwind(|| verify_hs256_jwt(&token, SECRET, now_sec));
        prop_assert!(res.is_ok(), "verify_hs256_jwt panicked (len={})", token.len());

        if let Some(claims) = res.unwrap() {
            prop_assert!(!claims.sid.is_empty());
            prop_assert!(claims.exp > now_sec);
        }
    }

    // Also exercise the "signature valid" path so we fuzz base64url decoding + JSON parsing logic
    // under the same conditions as real tokens.
    #[test]
    fn verify_gateway_session_token_never_panics_with_valid_signature(payload in proptest::collection::vec(any::<u8>(), 0..=4096)) {
        let token = mint_session_token(&payload, SECRET);
        let res = std::panic::catch_unwind(|| verify_gateway_session_token(&token, SECRET, 0));
        prop_assert!(res.is_ok(), "verify_gateway_session_token panicked for signed payload (len={})", payload.len());

        if let Some(verified) = res.unwrap() {
            prop_assert!(!verified.sid.is_empty());
        }
    }

    #[test]
    fn verify_hs256_jwt_never_panics_with_valid_signature(payload in proptest::collection::vec(any::<u8>(), 0..=4096)) {
        let token = mint_hs256_jwt(&payload, SECRET);
        let res = std::panic::catch_unwind(|| verify_hs256_jwt(&token, SECRET, 0));
        prop_assert!(res.is_ok(), "verify_hs256_jwt panicked for signed payload (len={})", payload.len());

        if let Some(claims) = res.unwrap() {
            prop_assert!(!claims.sid.is_empty());
            prop_assert!(claims.exp > 0);
        }
    }

    // Roundtrip property tests for syntactically-valid tokens that should verify successfully.
    #[test]
    fn session_token_valid_roundtrip(
        sid in ascii_ident(64),
        exp in prop_oneof![Just(i64::MAX), 1i64..=4_000_000_000i64],
        v_as_float in any::<bool>(),
    ) {
        let v = if v_as_float { json!(1.0) } else { json!(1) };
        let payload: Value = json!({ "v": v, "sid": sid, "exp": exp });
        let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

        let token = mint_session_token(&payload_bytes, SECRET);
        let verified = verify_gateway_session_token(&token, SECRET, 0)
            .expect("expected generated valid session token to verify");
        prop_assert_eq!(verified.sid, payload.get("sid").unwrap().as_str().unwrap());
        prop_assert_eq!(verified.exp_unix, exp);
    }

    #[test]
    fn jwt_valid_roundtrip(
        sid in ascii_ident(64),
        exp_delta in 1i64..=4_000_000_000i64,
        iat in 0i64..=4_000_000_000i64,
        origin in proptest::option::of(ascii_ident(32)),
        aud in proptest::option::of(ascii_ident(32)),
        iss in proptest::option::of(ascii_ident(32)),
        include_typ in any::<bool>(),
    ) {
        let now_sec = 0i64;
        let exp = now_sec + exp_delta;

        let mut obj = serde_json::Map::new();
        obj.insert("sid".to_owned(), Value::String(sid.clone()));
        obj.insert("exp".to_owned(), Value::Number(exp.into()));
        obj.insert("iat".to_owned(), Value::Number(iat.into()));
        if let Some(origin) = origin.clone() {
            obj.insert("origin".to_owned(), Value::String(origin));
        }
        if let Some(aud) = aud.clone() {
            obj.insert("aud".to_owned(), Value::String(aud));
        }
        if let Some(iss) = iss.clone() {
            obj.insert("iss".to_owned(), Value::String(iss));
        }
        let payload = Value::Object(obj);
        let payload_bytes = serde_json::to_vec(&payload).expect("serialize payload");

        let header = if include_typ {
            JWT_HEADER_JSON
        } else {
            JWT_HEADER_NO_TYP_JSON
        };
        let token = mint_hs256_jwt_with_header(header, &payload_bytes, SECRET);
        let claims =
            verify_hs256_jwt(&token, SECRET, now_sec).expect("expected generated valid jwt to verify");
        prop_assert_eq!(claims.sid, sid);
        prop_assert_eq!(claims.exp, exp);
        prop_assert_eq!(claims.iat, iat);
        prop_assert_eq!(claims.origin, origin);
        prop_assert_eq!(claims.aud, aud);
        prop_assert_eq!(claims.iss, iss);
    }
}

#[test]
fn extremely_long_inputs_do_not_panic() {
    // Ensure we remain robust against unusually large/malformed token strings without
    // turning this into an expensive test.
    let seg = "x".repeat(200_000);
    let token = format!("{seg}.{seg}.{seg}.{seg}");

    assert!(
        std::panic::catch_unwind(|| verify_gateway_session_token(&token, SECRET, 0)).is_ok(),
        "session token verification panicked on long input"
    );
    assert!(
        std::panic::catch_unwind(|| verify_hs256_jwt(&token, SECRET, 0)).is_ok(),
        "jwt verification panicked on long input"
    );
}
