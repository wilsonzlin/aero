use base64::{engine::general_purpose, Engine as _};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSession {
    pub sid: String,
    pub exp_unix: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub sid: String,
    pub exp: i64,
    pub iat: i64,
    pub origin: Option<String>,
    pub aud: Option<String>,
    pub iss: Option<String>,
}

const MAX_SESSION_TOKEN_PAYLOAD_B64_LEN: usize = 16 * 1024;
const HMAC_SHA256_SIG_LEN: usize = 32;
// Base64url without padding for a 32-byte HMAC:
// - base64 output is 44 chars (with one '=' padding)
// - without padding => 43 chars
const HMAC_SHA256_SIG_B64_LEN: usize = 43;
const MAX_SESSION_TOKEN_SIG_B64_LEN: usize = HMAC_SHA256_SIG_B64_LEN;
const MAX_JWT_HEADER_B64_LEN: usize = 4 * 1024;
const MAX_JWT_PAYLOAD_B64_LEN: usize = 16 * 1024;
const MAX_JWT_SIG_B64_LEN: usize = HMAC_SHA256_SIG_B64_LEN;
const MAX_SESSION_TOKEN_LEN: usize =
    MAX_SESSION_TOKEN_PAYLOAD_B64_LEN + 1 /* '.' */ + MAX_SESSION_TOKEN_SIG_B64_LEN;
const MAX_JWT_LEN: usize = MAX_JWT_HEADER_B64_LEN + 1 /* '.' */ + MAX_JWT_PAYLOAD_B64_LEN + 1 /* '.' */ + MAX_JWT_SIG_B64_LEN;

fn is_base64url(raw: &str, max_len: usize) -> bool {
    if raw.is_empty() {
        return false;
    }
    if raw.len() > max_len {
        return false;
    }
    // Base64url without padding cannot have length mod 4 == 1.
    if raw.len() % 4 == 1 {
        return false;
    }
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }

    let bytes = raw.as_bytes();
    if !bytes.iter().all(|&b| val(b).is_some()) {
        return false;
    }

    // Tighten validation to canonical base64url-no-pad. Even when the length is syntactically
    // valid (mod 4 != 1), the unused bits in the final base64 quantum must be zero.
    //
    // - len % 4 == 2 => 4 unused bits (must be zero)
    // - len % 4 == 3 => 2 unused bits (must be zero)
    match bytes.len() % 4 {
        0 => {}
        2 => {
            let last = val(bytes[bytes.len() - 1]).unwrap();
            if (last & 0x0f) != 0 {
                return false;
            }
        }
        3 => {
            let last = val(bytes[bytes.len() - 1]).unwrap();
            if (last & 0x03) != 0 {
                return false;
            }
        }
        _ => unreachable!("len%4==1 rejected above"),
    }

    true
}

fn decode_base64url_unchecked(raw: &str) -> Option<Vec<u8>> {
    general_purpose::URL_SAFE_NO_PAD.decode(raw).ok()
}

fn decode_hmac_sha256_sig_b64(raw: &str) -> Option<[u8; HMAC_SHA256_SIG_LEN]> {
    if raw.len() != HMAC_SHA256_SIG_B64_LEN {
        return None;
    }
    if !is_base64url(raw, HMAC_SHA256_SIG_B64_LEN) {
        return None;
    }

    let mut out = [0u8; HMAC_SHA256_SIG_LEN];
    let n = general_purpose::URL_SAFE_NO_PAD
        .decode_slice(raw, &mut out)
        .ok()?;
    if n != HMAC_SHA256_SIG_LEN {
        return None;
    }
    Some(out)
}

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

fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

pub fn verify_gateway_session_token(
    token: &str,
    secret: &[u8],
    now_ms: u64,
) -> Option<VerifiedSession> {
    // Quick coarse cap to avoid scanning attacker-controlled strings for delimiters.
    if token.len() > MAX_SESSION_TOKEN_LEN {
        return None;
    }
    let mut parts = token.split('.');
    let payload_b64 = parts.next()?;
    let sig_b64 = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    // Avoid doing any HMAC work for obviously-malformed tokens.
    if !is_base64url(payload_b64, MAX_SESSION_TOKEN_PAYLOAD_B64_LEN) {
        return None;
    }
    if sig_b64.len() != HMAC_SHA256_SIG_B64_LEN {
        return None;
    }

    let provided_sig = decode_hmac_sha256_sig_b64(sig_b64)?;
    let expected_sig = hmac_sha256(secret, &[payload_b64.as_bytes()]);
    if !constant_time_equal(&provided_sig, &expected_sig) {
        return None;
    }

    let payload_raw = decode_base64url_unchecked(payload_b64)?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_raw).ok()?;
    let obj = payload.as_object()?;

    // Gateway accepts any JSON number that parses to `1` (e.g. `1` or `1.0`),
    // since it compares the parsed JS number with `!== 1`.
    let v = obj.get("v")?.as_f64()?;
    if v != 1.0 {
        return None;
    }
    let sid = obj.get("sid")?.as_str()?;
    if sid.is_empty() {
        return None;
    }

    let exp_val = obj.get("exp")?;
    let exp = exp_val.as_f64()?;
    if !exp.is_finite() {
        return None;
    }

    let expires_at_ms = exp * 1000.0;
    if expires_at_ms <= now_ms as f64 {
        return None;
    }

    // The gateway treats `exp` as a JS `number`, but our public API exposes it as an `i64`. Avoid
    // silently saturating when casting large values that cannot fit in an `i64`.
    //
    // Note: For typical tokens issued by the gateway (`Math.floor(Date.now()/1000)+ttl`), `exp`
    // is always in-range.
    let exp_unix = match exp_val.as_i64() {
        Some(v) => v,
        None => match exp_val.as_u64() {
            Some(v) => i64::try_from(v).ok()?,
            None => {
                // Non-integer JSON number (e.g. `1.5`). Fall back to the JS semantics (`number`)
                // while still ensuring we can represent the value as an `i64`.
                // Note: `i64::MAX as f64` rounds up to `2^63` (since `2^63 - 1` is not exactly
                // representable in an `f64`). Use `>=` so we reject values that would otherwise
                // silently saturate to `i64::MAX` when casting from `f64`.
                if exp < i64::MIN as f64 || exp >= i64::MAX as f64 {
                    return None;
                }
                exp as i64
            }
        },
    };
    Some(VerifiedSession {
        sid: sid.to_owned(),
        exp_unix,
    })
}

pub fn verify_hs256_jwt(token: &str, secret: &[u8], now_sec: i64) -> Option<Claims> {
    // Quick coarse cap to avoid scanning attacker-controlled strings for delimiters.
    if token.len() > MAX_JWT_LEN {
        return None;
    }
    let mut parts = token.split('.');
    let header_b64 = parts.next()?;
    let payload_b64 = parts.next()?;
    let sig_b64 = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    // Avoid doing any HMAC work for obviously-malformed tokens.
    if !is_base64url(header_b64, MAX_JWT_HEADER_B64_LEN)
        || !is_base64url(payload_b64, MAX_JWT_PAYLOAD_B64_LEN)
        || sig_b64.len() != HMAC_SHA256_SIG_B64_LEN
    {
        return None;
    }

    let provided_sig = decode_hmac_sha256_sig_b64(sig_b64)?;
    let expected_sig = hmac_sha256(
        secret,
        &[header_b64.as_bytes(), b".", payload_b64.as_bytes()],
    );
    if !constant_time_equal(&provided_sig, &expected_sig) {
        return None;
    }

    // Signature is valid; now parse the JWT contents.
    let header_raw = decode_base64url_unchecked(header_b64)?;
    let header: serde_json::Value = serde_json::from_slice(&header_raw).ok()?;
    let header_obj = header.as_object()?;
    let alg = header_obj.get("alg")?.as_str()?;
    if alg != "HS256" {
        return None;
    }
    if let Some(typ_val) = header_obj.get("typ") {
        if !typ_val.is_string() {
            return None;
        }
    }

    let payload_raw = decode_base64url_unchecked(payload_b64)?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_raw).ok()?;
    let obj = payload.as_object()?;

    let sid = obj.get("sid")?.as_str()?;
    if sid.is_empty() {
        return None;
    }

    let exp = obj.get("exp")?.as_i64()?;
    let iat = obj.get("iat")?.as_i64()?;

    if now_sec >= exp {
        return None;
    }

    if let Some(nbf_val) = obj.get("nbf") {
        let nbf = nbf_val.as_i64()?;
        if now_sec < nbf {
            return None;
        }
    }

    let origin = match obj.get("origin") {
        None => None,
        Some(v) => Some(v.as_str()?.to_owned()),
    };
    let aud = match obj.get("aud") {
        None => None,
        Some(v) => Some(v.as_str()?.to_owned()),
    };
    let iss = match obj.get("iss") {
        None => None,
        Some(v) => Some(v.as_str()?.to_owned()),
    };

    Some(Claims {
        sid: sid.to_owned(),
        exp,
        iat,
        origin,
        aud,
        iss,
    })
}
