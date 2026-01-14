#![no_main]

use arbitrary::Unstructured;
use base64::{engine::general_purpose, Engine as _};
use libfuzzer_sys::fuzz_target;
use sha2::{Digest, Sha256};

use aero_auth_tokens::{verify_gateway_session_token, verify_hs256_jwt};

fn hmac_sha256(key: &[u8], chunks: &[&[u8]]) -> [u8; 32] {
    // HMAC-SHA256 as defined in RFC 2104.
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
    for chunk in chunks {
        inner.update(chunk);
    }
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let outer_hash = outer.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&outer_hash);
    out
}

fn b64url_val(b: u8) -> Option<u8> {
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

fn make_noncanonical_base64url(s: String) -> String {
    // Flip unused bits in the last base64 quantum. This produces a string which is still comprised
    // of base64url characters, but is invalid for canonical unpadded base64url.
    let rem = s.len() % 4;
    if rem != 2 && rem != 3 {
        return s;
    }
    let mut bytes = s.into_bytes();
    let last = bytes.len() - 1;
    let v = b64url_val(bytes[last]).unwrap_or(0);
    bytes[last] = b64url_char(v | 1);
    // Safety: we started from a valid UTF-8 string and only mutated one byte to another ASCII byte.
    String::from_utf8(bytes).expect("base64url should remain valid utf-8")
}

fn mint_session_token_b64(payload_b64: &str, secret: &[u8]) -> String {
    let sig = hmac_sha256(secret, &[payload_b64.as_bytes()]);
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{payload_b64}.{sig_b64}")
}

fn mint_session_token(payload: &[u8], secret: &[u8]) -> String {
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    mint_session_token_b64(&payload_b64, secret)
}

fn mint_hs256_jwt_b64(header_b64: &str, payload_b64: &str, secret: &[u8]) -> String {
    let sig = hmac_sha256(
        secret,
        &[header_b64.as_bytes(), b".", payload_b64.as_bytes()],
    );
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{header_b64}.{payload_b64}.{sig_b64}")
}

fn mint_hs256_jwt(header: &[u8], payload: &[u8], secret: &[u8]) -> String {
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
    mint_hs256_jwt_b64(&header_b64, &payload_b64, secret)
}

fn gen_ascii(u: &mut Unstructured<'_>, max_len: usize) -> String {
    let len: usize = u.int_in_range(0usize..=max_len).unwrap_or(0);
    let bytes = u.bytes(len).unwrap_or(&[]);
    let mut out = String::with_capacity(bytes.len());
    for b in bytes {
        out.push(char::from(b'a' + (b % 26)));
    }
    out
}

fn choose_num_str(u: &mut Unstructured<'_>) -> &'static str {
    // A mix of values that exercise i64 boundary handling and float parsing semantics.
    match u.arbitrary::<u8>().unwrap_or(0) % 10 {
        0 => "0",
        1 => "1",
        2 => "1.0",
        3 => "1.5",
        4 => "9223372036854775807",
        5 => "9223372036854775808",
        6 => "9223372036854775807.0",
        7 => "9223372036854775808.0",
        8 => "9223372036854774784.0",
        _ => "1e100",
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    let mode: u8 = u.arbitrary().unwrap_or(0);

    // Keep secrets bounded; we want to stress token parsing/validation, not spend cycles hashing a
    // multi-megabyte key.
    let secret_len: usize = u.int_in_range(0usize..=128).unwrap_or(0);
    let secret = u.bytes(secret_len).unwrap_or(&[]);

    match mode % 4 {
        // Arbitrary token strings (covers weird dots / invalid base64url etc).
        0 => {
            let rest_len = u.len();
            let token_bytes = u.bytes(rest_len).unwrap_or(&[]);
            let token = String::from_utf8_lossy(token_bytes).into_owned();

            let _ = verify_gateway_session_token(&token, secret, 0);
            let _ = verify_hs256_jwt(&token, secret, 0);
        }
        // Validly signed session token with arbitrary payload bytes.
        1 => {
            let payload_len: usize = u.int_in_range(0usize..=4096).unwrap_or(0);
            let payload = u.bytes(payload_len).unwrap_or(&[]);
            let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
            let payload_b64 = if u.arbitrary::<bool>().unwrap_or(false) {
                make_noncanonical_base64url(payload_b64)
            } else {
                payload_b64
            };
            let token = mint_session_token_b64(&payload_b64, secret);
            let _ = verify_gateway_session_token(&token, secret, 0);
        }
        // Validly signed JWT with arbitrary payload bytes, valid HS256 header.
        2 => {
            let payload_len: usize = u.int_in_range(0usize..=4096).unwrap_or(0);
            let payload = u.bytes(payload_len).unwrap_or(&[]);
            let header: &[u8] = if u.arbitrary::<bool>().unwrap_or(false) {
                br#"{"alg":"HS256","typ":"JWT"} "#
            } else {
                br#"{"alg":"HS256","typ":"JWT"}"#
            };
            let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header);
            let header_b64 = if u.arbitrary::<bool>().unwrap_or(false) {
                make_noncanonical_base64url(header_b64)
            } else {
                header_b64
            };
            let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload);
            let payload_b64 = if u.arbitrary::<bool>().unwrap_or(false) {
                make_noncanonical_base64url(payload_b64)
            } else {
                payload_b64
            };
            let token = mint_hs256_jwt_b64(&header_b64, &payload_b64, secret);
            let _ = verify_hs256_jwt(&token, secret, 0);
        }
        // Structured-ish JSON payloads to drive deeper parsing logic with valid signatures.
        _ => {
            // Session token JSON.
            let sid = gen_ascii(&mut u, 64);
            let exp = choose_num_str(&mut u);
            let v = if u.arbitrary::<bool>().unwrap_or(false) {
                "1"
            } else {
                "1.0"
            };
            let session_payload = format!(r#"{{"v":{v},"sid":"{sid}","exp":{exp}}}"#);
            let session_token = mint_session_token(session_payload.as_bytes(), secret);
            let _ = verify_gateway_session_token(&session_token, secret, 0);

            // JWT JSON.
            let sid = gen_ascii(&mut u, 64);
            let exp = choose_num_str(&mut u);
            let iat = choose_num_str(&mut u);
            let jwt_payload = format!(r#"{{"sid":"{sid}","exp":{exp},"iat":{iat}}}"#);
            let header = br#"{"alg":"HS256"}"#;
            let jwt_token = mint_hs256_jwt(header, jwt_payload.as_bytes(), secret);
            let _ = verify_hs256_jwt(&jwt_token, secret, 0);
        }
    }
});
