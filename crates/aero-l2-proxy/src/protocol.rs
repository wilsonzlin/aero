//! Protocol-level helpers that are shared between the proxy implementation and tests.
//!
//! The canonical wire framing is implemented in `crates/aero-l2-protocol`. This module contains
//! additional conventions used by the production proxy for `L2_TUNNEL_TYPE_ERROR` messages, as
//! defined in `docs/l2-tunnel-protocol.md`.

/// Protocol-level error codes carried inside `L2_TUNNEL_TYPE_ERROR` payloads.
///
/// Numeric values are stable and must not be changed once released.
pub const ERROR_CODE_PROTOCOL_ERROR: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_PROTOCOL_ERROR;
pub const ERROR_CODE_AUTH_REQUIRED: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_AUTH_REQUIRED;
pub const ERROR_CODE_AUTH_INVALID: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_AUTH_INVALID;
pub const ERROR_CODE_ORIGIN_MISSING: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_ORIGIN_MISSING;
pub const ERROR_CODE_ORIGIN_DENIED: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_ORIGIN_DENIED;
pub const ERROR_CODE_QUOTA_BYTES: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_QUOTA_BYTES;
pub const ERROR_CODE_QUOTA_FPS: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_QUOTA_FPS;
pub const ERROR_CODE_QUOTA_CONNECTIONS: u16 =
    aero_l2_protocol::L2_TUNNEL_ERROR_CODE_QUOTA_CONNECTIONS;
pub const ERROR_CODE_BACKPRESSURE: u16 = aero_l2_protocol::L2_TUNNEL_ERROR_CODE_BACKPRESSURE;

/// Encode an `ERROR` payload using the structured binary form.
///
/// This is a convenience wrapper around
/// [`aero_l2_protocol::encode_structured_error_payload`]. Prefer calling that
/// directly in new code.
pub fn encode_error_payload(code: u16, message: &str, max_payload_bytes: usize) -> Vec<u8> {
    aero_l2_protocol::encode_structured_error_payload(code, message, max_payload_bytes)
}

/// Attempt to decode a structured `ERROR` payload.
///
/// This is a convenience wrapper around
/// [`aero_l2_protocol::decode_structured_error_payload`].
pub fn decode_error_payload(payload: &[u8]) -> Option<(u16, String)> {
    aero_l2_protocol::decode_structured_error_payload(payload)
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
