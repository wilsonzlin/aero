//! Shared authentication token formats and verifiers.
//!
//! This crate is the canonical implementation for Aero's cross-language auth tokens. Token formats
//! and negative test cases are specified in the repository vectors:
//!
//! - `protocol-vectors/auth-tokens.json` (canonical format + rejection cases)
//! - `crates/conformance/test-vectors/aero-vectors-v1.json` (unified conformance vectors)
//!
//! ## Token formats
//!
//! ### Gateway session token (cookie)
//! Format: `<payload_b64url_no_pad>.<sig_b64url_no_pad>`
//!
//! - **payload**: UTF-8 JSON object containing at least `v` (version), `sid` (session id), and
//!   `exp` (expiry, seconds since epoch).
//! - **sig**: `HMAC-SHA256(secret, payload_b64url_no_pad)` encoded as base64url without padding.
//!
//! ### UDP relay JWT (HS256)
//! Format: `<header_b64url_no_pad>.<payload_b64url_no_pad>.<sig_b64url_no_pad>`
//!
//! - **header**: JSON object where `alg == "HS256"` and optional `typ` is a string.
//! - **payload**: JSON object containing `sid`, `iat`, `exp`, and optional `origin`/`aud`/`iss`.
//! - **sig**: `HMAC-SHA256(secret, header_b64url_no_pad "." payload_b64url_no_pad)`.
//!
//! ## Defensive parsing contract
//!
//! Verifiers are deliberately strict to avoid cross-language ambiguity and attacker-controlled
//! resource usage:
//!
//! - Reject non-canonical base64url-no-pad encodings (unused bits in the final quantum must be zero).
//! - Enforce coarse size caps before scanning/decoding.
//! - Verify signatures before decoding/parsing JSON payloads.
//! - Require exact delimiter counts (2-part session tokens, 3-part JWTs) and non-empty segments.
#![forbid(unsafe_code)]

use base64::{engine::general_purpose, Engine as _};
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Verified gateway session token claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSession {
    pub sid: String,
    pub exp_unix: i64,
}

/// Verified HS256 JWT claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub sid: String,
    pub exp: i64,
    pub iat: i64,
    pub origin: Option<String>,
    pub aud: Option<String>,
    pub iss: Option<String>,
}

/// Reasons a gateway session token can be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionTokenError {
    TooLong,
    InvalidFormat,
    InvalidBase64,
    InvalidJson,
    InvalidSignature,
    InvalidClaims,
    Expired,
}

impl std::fmt::Display for SessionTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::TooLong => "token too long",
            Self::InvalidFormat => "invalid token format",
            Self::InvalidBase64 => "invalid base64url",
            Self::InvalidJson => "invalid json",
            Self::InvalidSignature => "invalid signature",
            Self::InvalidClaims => "invalid claims",
            Self::Expired => "expired",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for SessionTokenError {}

/// Reasons an HS256 JWT can be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JwtError {
    TooLong,
    InvalidFormat,
    InvalidBase64,
    InvalidJson,
    UnsupportedAlg,
    InvalidSignature,
    InvalidClaims,
    Expired,
    NotYetValid,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::TooLong => "token too long",
            Self::InvalidFormat => "invalid token format",
            Self::InvalidBase64 => "invalid base64url",
            Self::InvalidJson => "invalid json",
            Self::UnsupportedAlg => "unsupported alg",
            Self::InvalidSignature => "invalid signature",
            Self::InvalidClaims => "invalid claims",
            Self::Expired => "expired",
            Self::NotYetValid => "not yet valid",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for JwtError {}

/// Reasons minting can fail (should be rare).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MintError {
    Json,
    OutOfRange,
    TooLong,
}

impl std::fmt::Display for MintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Json => "failed to serialize json",
            Self::OutOfRange => "value out of range",
            Self::TooLong => "token too long",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for MintError {}

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

#[derive(Debug, Serialize)]
struct SessionTokenPayload<'a> {
    v: u8,
    sid: &'a str,
    exp: i64,
}

pub fn mint_gateway_session_token_v1(
    sid: &str,
    exp_unix: i64,
    secret: &[u8],
) -> Result<String, MintError> {
    let payload = SessionTokenPayload {
        v: 1,
        sid,
        exp: exp_unix,
    };
    let payload_json = serde_json::to_vec(&payload).map_err(|_| MintError::Json)?;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
    if payload_b64.len() > MAX_SESSION_TOKEN_PAYLOAD_B64_LEN {
        return Err(MintError::TooLong);
    }

    let sig = hmac_sha256(secret, &[payload_b64.as_bytes()]);
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);

    let token = format!("{payload_b64}.{sig_b64}");
    if token.len() > MAX_SESSION_TOKEN_LEN {
        return Err(MintError::TooLong);
    }
    Ok(token)
}

#[derive(Debug, Serialize)]
struct JwtHeader<'a> {
    alg: &'a str,
    typ: &'a str,
}

#[derive(Debug, Serialize)]
struct JwtPayload<'a> {
    iat: i64,
    exp: i64,
    sid: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aud: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iss: Option<&'a str>,
}

pub fn mint_hs256_jwt(claims: &Claims, secret: &[u8]) -> Result<String, MintError> {
    let header = JwtHeader {
        alg: "HS256",
        typ: "JWT",
    };
    let header_json = serde_json::to_vec(&header).map_err(|_| MintError::Json)?;
    let header_b64 = general_purpose::URL_SAFE_NO_PAD.encode(header_json);
    if header_b64.len() > MAX_JWT_HEADER_B64_LEN {
        return Err(MintError::TooLong);
    }

    // Field order matters: it must match the shared protocol vectors and JS minting behavior.
    let payload = JwtPayload {
        iat: claims.iat,
        exp: claims.exp,
        sid: &claims.sid,
        origin: claims.origin.as_deref(),
        aud: claims.aud.as_deref(),
        iss: claims.iss.as_deref(),
    };
    let payload_json = serde_json::to_vec(&payload).map_err(|_| MintError::Json)?;
    let payload_b64 = general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
    if payload_b64.len() > MAX_JWT_PAYLOAD_B64_LEN {
        return Err(MintError::TooLong);
    }

    let sig = hmac_sha256(
        secret,
        &[header_b64.as_bytes(), b".", payload_b64.as_bytes()],
    );
    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(sig);

    let token = format!("{header_b64}.{payload_b64}.{sig_b64}");
    if token.len() > MAX_JWT_LEN {
        return Err(MintError::TooLong);
    }
    Ok(token)
}

pub fn verify_gateway_session_token(
    token: &str,
    secret: &[u8],
    now_ms: u64,
) -> Option<VerifiedSession> {
    verify_gateway_session_token_checked(token, secret, now_ms).ok()
}

pub fn verify_gateway_session_token_checked(
    token: &str,
    secret: &[u8],
    now_ms: u64,
) -> Result<VerifiedSession, SessionTokenError> {
    // Quick coarse cap to avoid scanning attacker-controlled strings for delimiters.
    if token.len() > MAX_SESSION_TOKEN_LEN {
        return Err(SessionTokenError::TooLong);
    }
    let mut parts = token.split('.');
    let payload_b64 = parts.next().ok_or(SessionTokenError::InvalidFormat)?;
    let sig_b64 = parts.next().ok_or(SessionTokenError::InvalidFormat)?;
    if parts.next().is_some() {
        return Err(SessionTokenError::InvalidFormat);
    }
    if payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(SessionTokenError::InvalidFormat);
    }

    // Avoid doing any HMAC work for obviously-malformed tokens.
    if !is_base64url(payload_b64, MAX_SESSION_TOKEN_PAYLOAD_B64_LEN) {
        return Err(SessionTokenError::InvalidBase64);
    }
    if sig_b64.len() != HMAC_SHA256_SIG_B64_LEN {
        return Err(SessionTokenError::InvalidBase64);
    }

    let provided_sig =
        decode_hmac_sha256_sig_b64(sig_b64).ok_or(SessionTokenError::InvalidBase64)?;
    let expected_sig = hmac_sha256(secret, &[payload_b64.as_bytes()]);
    if !constant_time_equal(&provided_sig, &expected_sig) {
        return Err(SessionTokenError::InvalidSignature);
    }

    let payload_raw =
        decode_base64url_unchecked(payload_b64).ok_or(SessionTokenError::InvalidBase64)?;
    let payload: serde_json::Value =
        serde_json::from_slice(&payload_raw).map_err(|_| SessionTokenError::InvalidJson)?;
    let obj = payload
        .as_object()
        .ok_or(SessionTokenError::InvalidJson)?;

    // Gateway accepts any JSON number that parses to `1` (e.g. `1` or `1.0`),
    // since it compares the parsed JS number with `!== 1`.
    let v = obj
        .get("v")
        .and_then(serde_json::Value::as_f64)
        .ok_or(SessionTokenError::InvalidClaims)?;
    if v != 1.0 {
        return Err(SessionTokenError::InvalidClaims);
    }
    let sid = obj
        .get("sid")
        .and_then(serde_json::Value::as_str)
        .ok_or(SessionTokenError::InvalidClaims)?;
    if sid.is_empty() {
        return Err(SessionTokenError::InvalidClaims);
    }

    let exp_val = obj.get("exp").ok_or(SessionTokenError::InvalidClaims)?;
    let exp = exp_val
        .as_f64()
        .ok_or(SessionTokenError::InvalidClaims)?;
    if !exp.is_finite() {
        return Err(SessionTokenError::InvalidClaims);
    }

    let expires_at_ms = exp * 1000.0;
    if expires_at_ms <= now_ms as f64 {
        return Err(SessionTokenError::Expired);
    }

    // The gateway treats `exp` as a JS `number`, but our public API exposes it as an `i64`. Avoid
    // silently saturating when casting large values that cannot fit in an `i64`.
    //
    // Note: For typical tokens issued by the gateway (`Math.floor(Date.now()/1000)+ttl`), `exp`
    // is always in-range.
    let exp_unix = match exp_val.as_i64() {
        Some(v) => v,
        None => match exp_val.as_u64() {
            Some(v) => i64::try_from(v)
                .map_err(|_| SessionTokenError::InvalidClaims)?,
            None => {
                // Non-integer JSON number (e.g. `1.5`). Fall back to the JS semantics (`number`)
                // while still ensuring we can represent the value as an `i64`.
                // Note: `i64::MAX as f64` rounds up to `2^63` (since `2^63 - 1` is not exactly
                // representable in an `f64`). Use `>=` so we reject values that would otherwise
                // silently saturate to `i64::MAX` when casting from `f64`.
                if exp < i64::MIN as f64 || exp >= i64::MAX as f64 {
                    return Err(SessionTokenError::InvalidClaims);
                }
                exp as i64
            }
        },
    };
    Ok(VerifiedSession {
        sid: sid.to_owned(),
        exp_unix,
    })
}

pub fn verify_hs256_jwt(token: &str, secret: &[u8], now_sec: i64) -> Option<Claims> {
    verify_hs256_jwt_checked(token, secret, now_sec).ok()
}

pub fn verify_hs256_jwt_checked(
    token: &str,
    secret: &[u8],
    now_sec: i64,
) -> Result<Claims, JwtError> {
    // Quick coarse cap to avoid scanning attacker-controlled strings for delimiters.
    if token.len() > MAX_JWT_LEN {
        return Err(JwtError::TooLong);
    }
    let mut parts = token.split('.');
    let header_b64 = parts.next().ok_or(JwtError::InvalidFormat)?;
    let payload_b64 = parts.next().ok_or(JwtError::InvalidFormat)?;
    let sig_b64 = parts.next().ok_or(JwtError::InvalidFormat)?;
    if parts.next().is_some() {
        return Err(JwtError::InvalidFormat);
    }
    if header_b64.is_empty() || payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(JwtError::InvalidFormat);
    }

    // Avoid doing any HMAC work for obviously-malformed tokens.
    if !is_base64url(header_b64, MAX_JWT_HEADER_B64_LEN)
        || !is_base64url(payload_b64, MAX_JWT_PAYLOAD_B64_LEN)
        || sig_b64.len() != HMAC_SHA256_SIG_B64_LEN
    {
        return Err(JwtError::InvalidBase64);
    }

    let provided_sig = decode_hmac_sha256_sig_b64(sig_b64).ok_or(JwtError::InvalidBase64)?;
    let expected_sig = hmac_sha256(
        secret,
        &[header_b64.as_bytes(), b".", payload_b64.as_bytes()],
    );
    if !constant_time_equal(&provided_sig, &expected_sig) {
        return Err(JwtError::InvalidSignature);
    }

    // Signature is valid; now parse the JWT contents.
    let header_raw = decode_base64url_unchecked(header_b64).ok_or(JwtError::InvalidBase64)?;
    let header: serde_json::Value =
        serde_json::from_slice(&header_raw).map_err(|_| JwtError::InvalidJson)?;
    let header_obj = header.as_object().ok_or(JwtError::InvalidJson)?;
    let alg = header_obj
        .get("alg")
        .and_then(serde_json::Value::as_str)
        .ok_or(JwtError::InvalidJson)?;
    if alg != "HS256" {
        return Err(JwtError::UnsupportedAlg);
    }
    if let Some(typ_val) = header_obj.get("typ") {
        if !typ_val.is_string() {
            return Err(JwtError::InvalidClaims);
        }
    }

    let payload_raw = decode_base64url_unchecked(payload_b64).ok_or(JwtError::InvalidBase64)?;
    let payload: serde_json::Value =
        serde_json::from_slice(&payload_raw).map_err(|_| JwtError::InvalidJson)?;
    let obj = payload.as_object().ok_or(JwtError::InvalidJson)?;

    let sid = obj
        .get("sid")
        .and_then(serde_json::Value::as_str)
        .ok_or(JwtError::InvalidClaims)?;
    if sid.is_empty() {
        return Err(JwtError::InvalidClaims);
    }

    let exp = obj
        .get("exp")
        .and_then(serde_json::Value::as_i64)
        .ok_or(JwtError::InvalidClaims)?;
    let iat = obj
        .get("iat")
        .and_then(serde_json::Value::as_i64)
        .ok_or(JwtError::InvalidClaims)?;

    if now_sec >= exp {
        return Err(JwtError::Expired);
    }

    if let Some(nbf_val) = obj.get("nbf") {
        let nbf = nbf_val
            .as_i64()
            .ok_or(JwtError::InvalidClaims)?;
        if now_sec < nbf {
            return Err(JwtError::NotYetValid);
        }
    }

    let origin = match obj.get("origin") {
        None => None,
        Some(v) => Some(v.as_str().ok_or(JwtError::InvalidClaims)?.to_owned()),
    };
    let aud = match obj.get("aud") {
        None => None,
        Some(v) => Some(v.as_str().ok_or(JwtError::InvalidClaims)?.to_owned()),
    };
    let iss = match obj.get("iss") {
        None => None,
        Some(v) => Some(v.as_str().ok_or(JwtError::InvalidClaims)?.to_owned()),
    };

    Ok(Claims {
        sid: sid.to_owned(),
        exp,
        iat,
        origin,
        aud,
        iss,
    })
}
