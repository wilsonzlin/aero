use aero_auth_tokens::{verify_gateway_session_token, verify_hs256_jwt};
use base64::{engine::general_purpose, Engine as _};
use sha2::{Digest, Sha256};

fn b64url_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

fn b64url_char(v: u8) -> u8 {
    match v {
        0..=25 => b'A' + v,
        26..=51 => b'a' + (v - 26),
        52..=61 => b'0' + (v - 52),
        62 => b'-',
        63 => b'_',
        _ => unreachable!("invalid base64url value {v}"),
    }
}

fn make_noncanonical_base64url(b64: String) -> String {
    let rem = b64.len() % 4;
    assert!(
        rem == 2 || rem == 3,
        "need mod4==2 or 3 to have unused bits, got {rem}"
    );
    let mut bytes = b64.into_bytes();
    let last = bytes.len() - 1;
    let v = b64url_value(bytes[last]).expect("base64url char");
    let v2 = match rem {
        2 => {
            assert_eq!(v & 0x0f, 0, "expected canonical last char for mod4==2");
            v | 1
        }
        3 => {
            assert_eq!(v & 0x03, 0, "expected canonical last char for mod4==3");
            v | 1
        }
        _ => unreachable!(),
    };
    bytes[last] = b64url_char(v2);
    String::from_utf8(bytes).expect("utf8")
}

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

#[test]
fn session_token_valid_signature_but_huge_non_json_payload_is_rejected_without_panicking() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    // Force the verifier to:
    // - accept the signature
    // - base64url-decode a large payload (near the verifier's size cap)
    // - reject it safely when JSON parsing fails
    //
    // Use a payload that decodes to lots of NUL bytes (never valid JSON).
    let payload_b64 = "A".repeat(16 * 1024);
    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{payload_b64}.{sig_b64}");

    let res = std::panic::catch_unwind(|| verify_gateway_session_token(&token, secret, now_ms));
    assert!(res.is_ok(), "verify_gateway_session_token panicked");
    assert!(
        res.unwrap().is_none(),
        "expected huge non-json payload to be rejected"
    );
}

#[test]
fn jwt_valid_signature_but_huge_non_json_payload_is_rejected_without_panicking() {
    let secret = b"unit-test-secret";
    let now_sec = 0;

    // See session_token_valid_signature_but_huge_non_json_payload_is_rejected_without_panicking.
    let header = br#"{"alg":"HS256"}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = "A".repeat(16 * 1024);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");

    let res = std::panic::catch_unwind(|| verify_hs256_jwt(&token, secret, now_sec));
    assert!(res.is_ok(), "verify_hs256_jwt panicked");
    assert!(
        res.unwrap().is_none(),
        "expected huge non-json payload to be rejected"
    );
}

#[test]
fn session_token_rejects_payloads_over_internal_size_cap() {
    let secret = b"unit-test-secret";

    // Over the verifier's size cap (keep multiple-of-4 so the only failure mode is the cap).
    let payload_b64 = "A".repeat(16 * 1024 + 4);
    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{payload_b64}.{sig_b64}");

    assert!(verify_gateway_session_token(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_payloads_over_internal_size_cap() {
    let secret = b"unit-test-secret";

    let header = br#"{"alg":"HS256"}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    // Over the verifier's size cap.
    let payload_b64 = "A".repeat(16 * 1024 + 4);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn session_token_rejects_signature_over_internal_size_cap() {
    let secret = b"unit-test-secret";

    let payload = br#"{"v":1,"sid":"abc","exp":12345}"#;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    // Over the verifier's signature cap (keep divisible-by-4 so it is well-formed base64).
    let sig_b64 = "A".repeat(44);
    let token = format!("{payload_b64}.{sig_b64}");

    assert!(verify_gateway_session_token(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_signature_over_internal_size_cap() {
    let secret = b"unit-test-secret";

    let header = br#"{"alg":"HS256"}"#;
    let payload = br#"{"sid":"abc","exp":1,"iat":0}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    // Over the verifier's signature cap.
    let sig_b64 = "A".repeat(44);
    let token = format!("{header_b64}.{payload_b64}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_header_over_internal_size_cap() {
    let secret = b"unit-test-secret";

    // Over the verifier's header cap.
    let header_b64 = "A".repeat(4 * 1024 + 4);
    let payload_b64 = "AA".repeat(8);
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(hmac_sha256(secret, b"dummy"));
    let token = format!("{header_b64}.{payload_b64}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn session_token_rejects_non_base64url_payload_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    // Base64url segment with an invalid character ('!'), but provide a matching signature for the
    // raw bytes to ensure we exercise the pre-validation path.
    let payload_b64 = "ab!d";
    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{payload_b64}.{sig_b64}");

    assert!(verify_gateway_session_token(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_non_base64url_payload_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    let header = br#"{"alg":"HS256"}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = "ab!d";
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_non_base64url_header_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    // Invalid base64url header (contains '!'), but provide a matching signature for the raw token
    // string so the verifier must reject via the header validation path.
    let header_b64 = "ab!d";
    let payload = br#"{"sid":"abc","exp":1,"iat":0}"#;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn session_token_rejects_base64url_length_mod4_eq_1_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    // Allowed base64url characters, but impossible length for base64-without-padding.
    let payload_b64 = "AAAAA"; // len=5 => mod 4 == 1
    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{payload_b64}.{sig_b64}");

    assert!(verify_gateway_session_token(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_payload_base64url_length_mod4_eq_1_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    let header = br#"{"alg":"HS256"}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);

    let payload_b64 = "AAAAA"; // len=5 => mod 4 == 1
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_header_base64url_length_mod4_eq_1_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    let header_b64 = "AAAAA"; // len=5 => mod 4 == 1
    let payload = br#"{"sid":"abc","exp":1,"iat":0}"#;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn session_token_rejects_noncanonical_base64url_payload_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    // Choose a payload length that produces a base64 string with unused bits (mod4==3).
    let payload = br#"{"v":1,"sid":"a","exp":12345}"#;
    let mut payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    assert_eq!(payload_b64.len() % 4, 3, "expected mod4==3");
    payload_b64 = make_noncanonical_base64url(payload_b64);

    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{payload_b64}.{sig_b64}");
    assert!(verify_gateway_session_token(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_noncanonical_base64url_payload_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    let header = br#"{"alg":"HS256"}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);

    // Trailing whitespace keeps the JSON valid and ensures mod4==2 for the base64 string.
    let payload = br#"{"sid":"abc","exp":12345,"iat":0} "#;
    let mut payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    assert_eq!(payload_b64.len() % 4, 2, "expected mod4==2");
    payload_b64 = make_noncanonical_base64url(payload_b64);

    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");
    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_noncanonical_base64url_header_even_if_signature_matches() {
    let secret = b"unit-test-secret";

    // Trailing whitespace keeps the JSON valid and ensures mod4==2 for the base64 string.
    let header = br#"{"alg":"HS256"} "#;
    let mut header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    assert_eq!(header_b64.len() % 4, 2, "expected mod4==2");
    header_b64 = make_noncanonical_base64url(header_b64);

    let payload = br#"{"sid":"abc","exp":12345,"iat":0}"#;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);

    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    let token = format!("{signing_input}.{sig_b64}");
    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn session_token_rejects_noncanonical_base64url_signature_even_if_hmac_matches() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    let payload = br#"{"v":1,"sid":"abc","exp":12345}"#;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    assert_eq!(sig_b64.len() % 4, 3, "expected signature b64 to be mod4==3");
    let sig_b64 = make_noncanonical_base64url(sig_b64);

    let token = format!("{payload_b64}.{sig_b64}");
    assert!(verify_gateway_session_token(&token, secret, now_ms).is_none());
}

#[test]
fn jwt_rejects_noncanonical_base64url_signature_even_if_hmac_matches() {
    let secret = b"unit-test-secret";
    let now_sec = 0;

    let header = br#"{"alg":"HS256"}"#;
    let payload = br#"{"sid":"abc","exp":12345,"iat":0}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    assert_eq!(sig_b64.len() % 4, 3, "expected signature b64 to be mod4==3");
    let sig_b64 = make_noncanonical_base64url(sig_b64);
    let token = format!("{signing_input}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, now_sec).is_none());
}

#[test]
fn session_token_rejects_deeply_nested_json_payload_without_panicking() {
    let secret = b"unit-test-secret";

    // Stress the JSON parser with very deep nesting. Even with a valid signature, verification
    // must fail safely (serde_json enforces a recursion limit to avoid stack overflow).
    let mut payload = String::new();
    for _ in 0..256 {
        payload.push('[');
    }
    payload.push('0');
    for _ in 0..256 {
        payload.push(']');
    }

    let token = mint_session_token(payload.as_bytes(), secret);
    assert!(verify_gateway_session_token(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_deeply_nested_json_payload_without_panicking() {
    let secret = b"unit-test-secret";

    let mut payload = String::new();
    for _ in 0..256 {
        payload.push('[');
    }
    payload.push('0');
    for _ in 0..256 {
        payload.push(']');
    }

    let header = br#"{"alg":"HS256","typ":"JWT"}"#;
    let token = mint_hs256_jwt(header, payload.as_bytes(), secret);
    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn session_token_rejects_signature_that_decodes_to_wrong_length() {
    let secret = b"unit-test-secret";

    let payload = br#"{"v":1,"sid":"abc","exp":12345}"#;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);

    // Valid base64url, but decodes to only 3 bytes.
    let sig_b64 = "AAAA";
    let token = format!("{payload_b64}.{sig_b64}");

    assert!(verify_gateway_session_token(&token, secret, 0).is_none());
}

#[test]
fn jwt_rejects_signature_that_decodes_to_wrong_length() {
    let secret = b"unit-test-secret";

    let header = br#"{"alg":"HS256"}"#;
    let payload = br#"{"sid":"abc","exp":1,"iat":0}"#;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);

    let sig_b64 = "AAAA";
    let token = format!("{header_b64}.{payload_b64}.{sig_b64}");

    assert!(verify_hs256_jwt(&token, secret, 0).is_none());
}

#[test]
fn session_token_accepts_long_secret_over_hmac_block_len() {
    let secret = vec![b'x'; 100];
    let now_ms = 0;

    let payload = br#"{"v":1,"sid":"abc","exp":12345}"#;
    let token = mint_session_token(payload, &secret);
    let verified =
        verify_gateway_session_token(&token, &secret, now_ms).expect("expected token to verify");
    assert_eq!(verified.sid, "abc");
    assert_eq!(verified.exp_unix, 12345);

    assert!(
        verify_gateway_session_token(&token, b"wrong-secret", now_ms).is_none(),
        "expected signature mismatch to be rejected"
    );
}

#[test]
fn jwt_accepts_long_secret_over_hmac_block_len() {
    let secret = vec![b'x'; 100];
    let now_sec = 0;

    let header = br#"{"alg":"HS256"}"#;
    let payload = br#"{"sid":"abc","exp":12345,"iat":0}"#;
    let token = mint_hs256_jwt(header, payload, &secret);
    let claims = verify_hs256_jwt(&token, &secret, now_sec).expect("expected jwt to verify");
    assert_eq!(claims.sid, "abc");
    assert_eq!(claims.exp, 12345);
    assert_eq!(claims.iat, 0);

    assert!(
        verify_hs256_jwt(&token, b"wrong-secret", now_sec).is_none(),
        "expected signature mismatch to be rejected"
    );
}

#[test]
fn session_token_accepts_empty_secret() {
    let secret = b"";
    let now_ms = 0;

    let payload = br#"{"v":1,"sid":"abc","exp":12345}"#;
    let token = mint_session_token(payload, secret);
    let verified =
        verify_gateway_session_token(&token, secret, now_ms).expect("expected token to verify");
    assert_eq!(verified.sid, "abc");
    assert_eq!(verified.exp_unix, 12345);
}

#[test]
fn jwt_accepts_empty_secret() {
    let secret = b"";
    let now_sec = 0;

    let header = br#"{"alg":"HS256"}"#;
    let payload = br#"{"sid":"abc","exp":12345,"iat":0}"#;
    let token = mint_hs256_jwt(header, payload, secret);
    let claims = verify_hs256_jwt(&token, secret, now_sec).expect("expected jwt to verify");
    assert_eq!(claims.sid, "abc");
    assert_eq!(claims.exp, 12345);
    assert_eq!(claims.iat, 0);
}
