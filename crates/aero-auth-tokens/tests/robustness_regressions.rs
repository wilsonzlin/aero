use aero_auth_tokens::{verify_gateway_session_token, verify_hs256_jwt};
use base64::{engine::general_purpose, Engine as _};
use sha2::{Digest, Sha256};

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

fn mint_hs256_jwt(header: &[u8], payload: &[u8], secret: &[u8]) -> String {
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{signing_input}.{sig_b64}")
}

#[test]
fn verify_session_token_rejects_exp_beyond_i64_when_float() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    // `2^63` is out of range for `i64` and must be rejected (even if represented as a float).
    let payload = br#"{"v":1,"sid":"abc","exp":9223372036854775808.0}"#;
    let token = mint_session_token(payload, secret);
    assert!(
        verify_gateway_session_token(&token, secret, now_ms).is_none(),
        "expected exp beyond i64 (float) to be rejected"
    );
}

#[test]
fn verify_session_token_accepts_large_exp_just_below_i64_max_when_float() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    // The largest `f64` < `2^63` that is still a whole number and representable; should verify.
    let payload = br#"{"v":1,"sid":"abc","exp":9223372036854774784.0}"#;
    let token = mint_session_token(payload, secret);
    let verified = verify_gateway_session_token(&token, secret, now_ms)
        .expect("expected token with large in-range float exp to verify");
    assert_eq!(verified.exp_unix, 9223372036854774784);
}

#[test]
fn verify_session_token_rejects_exp_i64_max_rendered_as_float() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    // `i64::MAX` is not exactly representable as an `f64`; serde_json will round this to `2^63`,
    // which would otherwise silently saturate to `i64::MAX` when casting. Reject instead.
    let payload = br#"{"v":1,"sid":"abc","exp":9223372036854775807.0}"#;
    let token = mint_session_token(payload, secret);
    assert!(
        verify_gateway_session_token(&token, secret, now_ms).is_none(),
        "expected exp=i64::MAX (float) to be rejected due to rounding"
    );
}

#[test]
fn verify_session_token_rejects_extremely_large_exp_float() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    // Extremely large but finite float should be rejected because it cannot fit in i64.
    let payload = br#"{"v":1,"sid":"abc","exp":1e100}"#;
    let token = mint_session_token(payload, secret);
    assert!(
        verify_gateway_session_token(&token, secret, now_ms).is_none(),
        "expected extremely large exp float to be rejected"
    );
}

#[test]
fn verify_jwt_rejects_exp_beyond_i64() {
    let secret = b"unit-test-secret";
    let now_sec = 0;

    // `2^63` fits in u64, but not in i64; JWT verifier requires exp as i64.
    let header = br#"{"alg":"HS256","typ":"JWT"}"#;
    let payload = br#"{"sid":"abc","exp":9223372036854775808,"iat":0}"#;
    let token = mint_hs256_jwt(header, payload, secret);
    assert!(
        verify_hs256_jwt(&token, secret, now_sec).is_none(),
        "expected jwt exp beyond i64 to be rejected"
    );
}

#[test]
fn session_token_rejects_malformed_json_payload_even_with_valid_signature() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    // Missing closing brace.
    let payload = br#"{"v":1,"sid":"abc","exp":123"#;
    let token = mint_session_token(payload, secret);
    assert!(
        verify_gateway_session_token(&token, secret, now_ms).is_none(),
        "expected malformed-json payload to be rejected"
    );
}

#[test]
fn jwt_rejects_malformed_json_payload_even_with_valid_signature() {
    let secret = b"unit-test-secret";
    let now_sec = 0;

    let header = br#"{"alg":"HS256","typ":"JWT"}"#;
    // Missing closing brace.
    let payload = br#"{"sid":"abc","exp":123,"iat":0"#;
    let token = mint_hs256_jwt(header, payload, secret);
    assert!(
        verify_hs256_jwt(&token, secret, now_sec).is_none(),
        "expected malformed-json payload to be rejected"
    );
}
