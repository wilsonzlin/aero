use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClaims {
    pub sid: String,
    pub exp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionVerifyError {
    InvalidFormat,
    InvalidBase64,
    InvalidSignature,
    InvalidJson,
    InvalidClaims,
    Expired,
}

#[derive(Debug, Serialize)]
struct SessionTokenPayload<'a> {
    v: u8,
    sid: &'a str,
    exp: u64,
}

pub fn mint_session_token(claims: &SessionClaims, secret: &[u8]) -> String {
    let payload = SessionTokenPayload {
        v: 1,
        sid: &claims.sid,
        exp: claims.exp,
    };
    let payload_json = serde_json::to_vec(&payload).expect("serialize session token payload");
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);

    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let sig = hmac::sign(&key, payload_b64.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig.as_ref());

    format!("{payload_b64}.{sig_b64}")
}

pub fn verify_session_token(
    token: &str,
    secret: &[u8],
    now_ms: u64,
) -> Result<SessionClaims, SessionVerifyError> {
    let mut parts = token.split('.');
    let payload_b64 = parts.next().ok_or(SessionVerifyError::InvalidFormat)?;
    let sig_b64 = parts.next().ok_or(SessionVerifyError::InvalidFormat)?;
    if parts.next().is_some() {
        return Err(SessionVerifyError::InvalidFormat);
    }
    if payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(SessionVerifyError::InvalidFormat);
    }

    let sig = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|_| SessionVerifyError::InvalidBase64)?;

    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::verify(&key, payload_b64.as_bytes(), &sig)
        .map_err(|_| SessionVerifyError::InvalidSignature)?;

    let payload_raw = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| SessionVerifyError::InvalidBase64)?;

    let payload: Value =
        serde_json::from_slice(&payload_raw).map_err(|_| SessionVerifyError::InvalidJson)?;
    let obj = payload.as_object().ok_or(SessionVerifyError::InvalidJson)?;

    // Gateway session tokens are authored by `backend/aero-gateway/src/session.ts`.
    //
    // That implementation treats `v` as a JS number and compares it with `!== 1`, which means any
    // JSON number that parses to `1` (e.g. `1` or `1.0`) is accepted. Mirror that behavior so the
    // proxy verifier stays byte-for-byte compatible with the gateway.
    let version = obj
        .get("v")
        .and_then(Value::as_f64)
        .ok_or(SessionVerifyError::InvalidClaims)?;
    if version != 1.0 {
        return Err(SessionVerifyError::InvalidClaims);
    }

    let sid = obj
        .get("sid")
        .and_then(Value::as_str)
        .filter(|sid| !sid.is_empty())
        .ok_or(SessionVerifyError::InvalidClaims)?;

    let exp = obj
        .get("exp")
        .and_then(Value::as_f64)
        .filter(|exp| exp.is_finite())
        .ok_or(SessionVerifyError::InvalidClaims)?;
    // Match the gateway semantics: exp is expressed in seconds (unix time), and expiration is
    // checked by comparing exp*1000 with Date.now() milliseconds.
    let expires_at_ms = exp * 1000.0;
    if expires_at_ms <= now_ms as f64 {
        return Err(SessionVerifyError::Expired);
    }

    let exp = if exp >= 0.0 && exp <= u64::MAX as f64 {
        exp as u64
    } else {
        return Err(SessionVerifyError::InvalidClaims);
    };

    Ok(SessionClaims {
        sid: sid.to_string(),
        exp,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayJwtClaims {
    pub iat: u64,
    pub exp: u64,
    pub sid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtVerifyError {
    InvalidFormat,
    InvalidBase64,
    InvalidJson,
    UnsupportedAlg,
    InvalidSignature,
    InvalidClaims,
    Expired,
    NotYetValid,
}

#[derive(Debug, Serialize)]
struct JwtHeader<'a> {
    alg: &'a str,
    typ: &'a str,
}

pub fn mint_relay_jwt_hs256(claims: &RelayJwtClaims, secret: &[u8]) -> String {
    let header = JwtHeader {
        alg: "HS256",
        typ: "JWT",
    };
    let header_json = serde_json::to_vec(&header).expect("serialize jwt header");
    let header_b64 = URL_SAFE_NO_PAD.encode(header_json);

    let payload_json = serde_json::to_vec(claims).expect("serialize jwt payload");
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);

    let signing_input = format!("{header_b64}.{payload_b64}");
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let sig = hmac::sign(&key, signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig.as_ref());

    format!("{signing_input}.{sig_b64}")
}

pub fn verify_relay_jwt_hs256(
    token: &str,
    secret: &[u8],
    now_unix: u64,
) -> Result<RelayJwtClaims, JwtVerifyError> {
    let mut parts = token.split('.');
    let header_b64 = parts.next().ok_or(JwtVerifyError::InvalidFormat)?;
    let payload_b64 = parts.next().ok_or(JwtVerifyError::InvalidFormat)?;
    let sig_b64 = parts.next().ok_or(JwtVerifyError::InvalidFormat)?;
    if parts.next().is_some() {
        return Err(JwtVerifyError::InvalidFormat);
    }
    if header_b64.is_empty() || payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(JwtVerifyError::InvalidFormat);
    }

    let header_raw = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| JwtVerifyError::InvalidBase64)?;

    let header: Value =
        serde_json::from_slice(&header_raw).map_err(|_| JwtVerifyError::InvalidJson)?;
    let header_obj = header.as_object().ok_or(JwtVerifyError::InvalidJson)?;
    let alg = header_obj
        .get("alg")
        .and_then(Value::as_str)
        .ok_or(JwtVerifyError::InvalidJson)?;
    if alg != "HS256" {
        return Err(JwtVerifyError::UnsupportedAlg);
    }
    // Reject tokens that carry a non-string `typ` value (including explicit `null`). This mirrors
    // the shared protocol vectors in `protocol-vectors/auth-tokens.json` and hardens parsing
    // against weird/malformed header encodings.
    if let Some(typ) = header_obj.get("typ") {
        if !typ.is_string() {
            return Err(JwtVerifyError::InvalidClaims);
        }
    }

    let sig = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|_| JwtVerifyError::InvalidBase64)?;

    let signing_input = format!("{header_b64}.{payload_b64}");
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::verify(&key, signing_input.as_bytes(), &sig)
        .map_err(|_| JwtVerifyError::InvalidSignature)?;

    let payload_raw = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| JwtVerifyError::InvalidBase64)?;
    let claims: RelayJwtClaims =
        serde_json::from_slice(&payload_raw).map_err(|_| JwtVerifyError::InvalidJson)?;

    if claims.sid.is_empty() {
        return Err(JwtVerifyError::InvalidClaims);
    }
    if now_unix >= claims.exp {
        return Err(JwtVerifyError::Expired);
    }
    if let Some(nbf) = claims.nbf {
        if now_unix < nbf {
            return Err(JwtVerifyError::NotYetValid);
        }
    }

    Ok(claims)
}
