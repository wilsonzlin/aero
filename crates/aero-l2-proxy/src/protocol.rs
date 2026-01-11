//! Protocol-level helpers that are shared between the proxy implementation and tests.
//!
//! The canonical wire framing is implemented in `crates/aero-l2-protocol`. This module contains
//! additional conventions used by the production proxy (structured ERROR payloads), as defined in
//! `docs/l2-tunnel-protocol.md`.

/// Protocol-level error codes carried inside `L2_TUNNEL_TYPE_ERROR` payloads.
///
/// Numeric values are stable and must not be changed once released.
pub const ERROR_CODE_PROTOCOL_ERROR: u16 = 1;
pub const ERROR_CODE_AUTH_REQUIRED: u16 = 2;
pub const ERROR_CODE_AUTH_INVALID: u16 = 3;
pub const ERROR_CODE_ORIGIN_MISSING: u16 = 4;
pub const ERROR_CODE_ORIGIN_DENIED: u16 = 5;
pub const ERROR_CODE_QUOTA_BYTES: u16 = 6;
pub const ERROR_CODE_QUOTA_FPS: u16 = 7;
pub const ERROR_CODE_QUOTA_CONNECTIONS: u16 = 8;
pub const ERROR_CODE_BACKPRESSURE: u16 = 9;

/// Encode an `ERROR` payload using the structured binary form:
///
/// ```text
/// code (u16 BE) | msg_len (u16 BE) | msg (msg_len bytes, UTF-8)
/// ```
///
/// The returned payload is truncated as needed to fit within `max_payload_bytes`.
pub fn encode_error_payload(code: u16, message: &str, max_payload_bytes: usize) -> Vec<u8> {
    // Need space for the 4-byte header (code + msg_len). If the configured max is below that,
    // return an empty payload so callers can still emit an ERROR control message within their
    // configured limits (the peer will still see an ERROR type, even if it cannot decode a
    // structured code/message).
    if max_payload_bytes < 4 {
        return Vec::new();
    }
    // Need space for the 4-byte header (code + msg_len).
    let max_msg_len = max_payload_bytes.saturating_sub(4).min(u16::MAX as usize);

    let msg_bytes = message.as_bytes();
    let mut msg_len = msg_bytes.len().min(max_msg_len);
    while msg_len > 0 && !message.is_char_boundary(msg_len) {
        msg_len -= 1;
    }
    let msg_bytes = &msg_bytes[..msg_len];

    let msg_len: u16 = msg_bytes.len().try_into().unwrap_or(u16::MAX);

    let mut out = Vec::with_capacity(4 + msg_bytes.len());
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&msg_len.to_be_bytes());
    out.extend_from_slice(msg_bytes);
    out
}

/// Attempt to decode a structured `ERROR` payload.
///
/// Returns `(code, message)` only if the payload matches the exact structured encoding.
pub fn decode_error_payload(payload: &[u8]) -> Option<(u16, String)> {
    let header: [u8; 4] = payload.get(..4)?.try_into().ok()?;
    let code = u16::from_be_bytes([header[0], header[1]]);
    let msg_len = u16::from_be_bytes([header[2], header[3]]) as usize;
    if payload.len() != 4usize.checked_add(msg_len)? {
        return None;
    }
    let msg_bytes = payload.get(4..)?;
    let msg = String::from_utf8(msg_bytes.to_vec()).ok()?;
    Some((code, msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_error_payload_respects_max_payload_bytes() {
        // Underflow: cannot fit the structured header, so return an empty payload.
        assert_eq!(encode_error_payload(1, "hi", 0).len(), 0);
        assert_eq!(encode_error_payload(1, "hi", 3).len(), 0);

        // Exactly enough space for the structured header, but no message bytes.
        assert_eq!(encode_error_payload(1, "hi", 4).len(), 4);

        // Message is truncated to fit and must respect UTF-8 boundaries.
        let payload = encode_error_payload(1, "hi", 5);
        assert_eq!(payload.len(), 5);

        // Emoji is 4 bytes; if only 1 byte is available for the message, it must be dropped.
        let payload = encode_error_payload(1, "😃", 5);
        assert_eq!(payload.len(), 4);
    }
}
