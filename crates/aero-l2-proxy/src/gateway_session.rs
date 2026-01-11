use axum::http::HeaderValue;

pub const SESSION_COOKIE_NAME: &str = "aero_session";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSession {
    pub id: String,
    pub expires_at_ms: u64,
}

/// Verify an Aero Gateway session token.
///
/// This must match `backend/aero-gateway/src/session.ts`:
/// - Token format: `<payload_b64url>.<sig_b64url>`
/// - Signature: `HMAC_SHA256(secret, payload_b64url)`
/// - base64url: `-`/`_`, no padding
/// - Payload JSON: `{ v: 1, sid: string, exp: number }` (`exp` is seconds since unix epoch)
/// - Reject expired sessions (`exp*1000 <= now_ms`) and malformed tokens.
pub fn verify_session_token(token: &str, secret: &[u8], now_ms: u64) -> Option<VerifiedSession> {
    let claims = crate::auth::verify_session_token(token, secret, now_ms).ok()?;
    Some(VerifiedSession {
        expires_at_ms: claims.exp.checked_mul(1000)?,
        id: claims.sid,
    })
}

/// Extracts and verifies the `aero_session` cookie from a `Cookie` header.
pub fn verify_session_cookie(
    cookie_header: &HeaderValue,
    secret: &[u8],
    now_ms: u64,
) -> Option<VerifiedSession> {
    let token = extract_session_cookie_value(cookie_header)?;
    verify_session_token(&token, secret, now_ms)
}

pub(crate) fn extract_session_cookie_value(cookie_header: &HeaderValue) -> Option<String> {
    let raw = cookie_header.to_str().ok()?;
    extract_cookie_value(raw, SESSION_COOKIE_NAME)
}

fn extract_cookie_value(raw: &str, name: &str) -> Option<String> {
    for part in raw.split(';') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let idx = trimmed.find('=')?;
        if idx == 0 {
            continue;
        }
        let key = trimmed[..idx].trim();
        if key != name {
            continue;
        }
        let value = trimmed[idx + 1..].trim();
        if value.is_empty() {
            return None;
        }
        return Some(percent_decode(value));
    }
    None
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = from_hex(bytes[i + 1]);
            let lo = from_hex(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
