use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ring::hmac;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Deserialize)]
struct SessionTokenPayloadOwned {
    v: u8,
    sid: String,
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
    let (payload_b64, sig_b64) = token
        .split_once('.')
        .ok_or(SessionVerifyError::InvalidFormat)?;
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

    let payload: SessionTokenPayloadOwned =
        serde_json::from_slice(&payload_raw).map_err(|_| SessionVerifyError::InvalidJson)?;

    if payload.v != 1 || payload.sid.is_empty() {
        return Err(SessionVerifyError::InvalidClaims);
    }

    let expires_at_ms = payload
        .exp
        .checked_mul(1000)
        .ok_or(SessionVerifyError::InvalidClaims)?;
    if expires_at_ms <= now_ms {
        return Err(SessionVerifyError::Expired);
    }

    Ok(SessionClaims {
        sid: payload.sid,
        exp: payload.exp,
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

#[derive(Debug, Deserialize)]
struct JwtHeaderOwned {
    alg: String,
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

    let header_raw = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| JwtVerifyError::InvalidBase64)?;
    let header: JwtHeaderOwned =
        serde_json::from_slice(&header_raw).map_err(|_| JwtVerifyError::InvalidJson)?;
    if header.alg != "HS256" {
        return Err(JwtVerifyError::UnsupportedAlg);
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
