use aero_auth_tokens::{
    mint_gateway_session_token_v1, mint_hs256_jwt, verify_gateway_session_token_checked,
    verify_hs256_jwt_checked, Claims, JwtError, MintError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClaims {
    pub sid: String,
    pub exp: u64,
}

pub fn mint_session_token(claims: &SessionClaims, secret: &[u8]) -> Result<String, MintError> {
    let exp_unix = i64::try_from(claims.exp).map_err(|_| MintError::OutOfRange)?;
    mint_gateway_session_token_v1(&claims.sid, exp_unix, secret)
}

pub fn verify_session_token(token: &str, secret: &[u8], now_ms: u64) -> Option<SessionClaims> {
    let verified = verify_gateway_session_token_checked(token, secret, now_ms).ok()?;
    let exp = u64::try_from(verified.exp_unix).ok()?;
    Some(SessionClaims {
        sid: verified.sid,
        exp,
    })
}

pub type RelayJwtClaims = Claims;
pub type JwtVerifyError = JwtError;

pub fn mint_relay_jwt_hs256(claims: &RelayJwtClaims, secret: &[u8]) -> Result<String, MintError> {
    mint_hs256_jwt(claims, secret)
}

pub fn verify_relay_jwt_hs256(
    token: &str,
    secret: &[u8],
    now_unix: u64,
) -> Result<RelayJwtClaims, JwtVerifyError> {
    let now_sec = i64::try_from(now_unix).map_err(|_| JwtVerifyError::InvalidClaims)?;
    verify_hs256_jwt_checked(token, secret, now_sec)
}
