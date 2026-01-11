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

fn decode_base64url(raw: &str) -> Option<Vec<u8>> {
    if raw.is_empty() {
        return None;
    }
    if !raw.as_bytes().iter().all(|b| {
        matches!(
            b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'
        )
    }) {
        return None;
    }
    general_purpose::URL_SAFE_NO_PAD.decode(raw).ok()
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
    let mut parts = token.split('.');
    let payload_b64 = parts.next()?;
    let sig_b64 = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let provided_sig = decode_base64url(sig_b64)?;
    let expected_sig = hmac_sha256(secret, &[payload_b64.as_bytes()]);
    if !constant_time_equal(&provided_sig, &expected_sig) {
        return None;
    }

    let payload_raw = decode_base64url(payload_b64)?;
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

    let exp_unix = exp as i64;
    Some(VerifiedSession {
        sid: sid.to_owned(),
        exp_unix,
    })
}

pub fn verify_hs256_jwt(token: &str, secret: &[u8], now_sec: i64) -> Option<Claims> {
    let mut parts = token.split('.');
    let header_b64 = parts.next()?;
    let payload_b64 = parts.next()?;
    let sig_b64 = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let header_raw = decode_base64url(header_b64)?;
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

    let provided_sig = decode_base64url(sig_b64)?;
    let expected_sig = hmac_sha256(
        secret,
        &[header_b64.as_bytes(), b".", payload_b64.as_bytes()],
    );
    if !constant_time_equal(&provided_sig, &expected_sig) {
        return None;
    }

    let payload_raw = decode_base64url(payload_b64)?;
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
