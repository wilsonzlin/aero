use aero_auth_tokens::verify_gateway_session_token;
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

fn mint_session_token(payload_json: &str, secret: &[u8]) -> String {
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    let sig = hmac_sha256(secret, payload_b64.as_bytes());
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{payload_b64}.{sig_b64}")
}

#[test]
fn verify_session_token_rejects_exp_beyond_i64() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    // 2^63, one second beyond `i64::MAX`.
    let payload = format!(r#"{{"v":1,"sid":"abc","exp":{}}}"#, (i64::MAX as u64) + 1);
    let token = mint_session_token(&payload, secret);
    assert!(
        verify_gateway_session_token(&token, secret, now_ms).is_none(),
        "expected exp beyond i64 to be rejected"
    );
}

#[test]
fn verify_session_token_accepts_exp_at_i64_max() {
    let secret = b"unit-test-secret";
    let now_ms = 0;

    let payload = format!(r#"{{"v":1,"sid":"abc","exp":{}}}"#, i64::MAX);
    let token = mint_session_token(&payload, secret);
    let verified = verify_gateway_session_token(&token, secret, now_ms)
        .expect("expected token with exp=i64::MAX to verify");
    assert_eq!(verified.exp_unix, i64::MAX);
}
