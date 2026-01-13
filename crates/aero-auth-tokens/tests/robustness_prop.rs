#![cfg(not(target_arch = "wasm32"))]

use aero_auth_tokens::{verify_gateway_session_token, verify_hs256_jwt};
use base64::{engine::general_purpose, Engine as _};
use proptest::prelude::*;
use sha2::{Digest, Sha256};

const SECRET: &[u8] = b"prop-test-secret";
const JWT_HEADER_JSON: &[u8] = br#"{"alg":"HS256","typ":"JWT"}"#;

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
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(JWT_HEADER_JSON);
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{signing_input}.{sig_b64}")
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
