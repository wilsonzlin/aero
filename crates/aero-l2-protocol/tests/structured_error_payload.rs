use aero_l2_protocol::{
    decode_structured_error_payload, encode_structured_error_payload,
    L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN,
};

#[test]
fn encode_structured_error_payload_underflow_returns_empty_payload() {
    assert!(encode_structured_error_payload(1, "hi", 0).is_empty());
    assert!(
        encode_structured_error_payload(1, "hi", L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN - 1)
            .is_empty()
    );
}

#[test]
fn encode_structured_error_payload_header_only_when_no_space_for_message() {
    let payload =
        encode_structured_error_payload(0x1234, "hello", L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN);
    assert_eq!(payload.len(), L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN);
    assert_eq!(&payload[..2], &0x1234u16.to_be_bytes());
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 0);
}

#[test]
fn encode_structured_error_payload_truncates_ascii_to_fit() {
    let max = L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN + 2;
    let payload = encode_structured_error_payload(0x1234, "hello", max);
    assert_eq!(payload.len(), max);
    assert_eq!(&payload[..2], &0x1234u16.to_be_bytes());
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 2);
    assert_eq!(&payload[L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN..], b"he");
}

#[test]
fn encode_structured_error_payload_drops_partial_utf8_char() {
    // 😃 is 4 bytes; allow only a single byte for message => must drop it entirely.
    let max = L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN + 1;
    let payload = encode_structured_error_payload(1, "😃", max);
    assert_eq!(payload.len(), L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN);
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 0);
}

#[test]
fn encode_structured_error_payload_keeps_complete_utf8_char_at_boundary() {
    // "a😃b" is 6 bytes. Allow 5 => should keep "a😃" (exactly on a char boundary).
    let max = L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN + 5;
    let payload = encode_structured_error_payload(1, "a😃b", max);
    assert_eq!(payload.len(), max);
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 5);
    assert_eq!(
        &payload[L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN..],
        "a😃".as_bytes()
    );
}

#[test]
fn encode_structured_error_payload_steps_back_to_utf8_boundary() {
    // "a😃b" is 6 bytes; allow 4 => must step back to the previous boundary (1), producing "a".
    let max = L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN + 4;
    let payload = encode_structured_error_payload(1, "a😃b", max);
    assert_eq!(payload.len(), L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN + 1);
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 1);
    assert_eq!(&payload[L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN..], b"a");
}

#[test]
fn decode_structured_error_payload_roundtrips_valid_payload() {
    let payload = encode_structured_error_payload(9, "backpressure", usize::MAX);
    let (code, msg) = decode_structured_error_payload(&payload).expect("decode");
    assert_eq!(code, 9);
    assert_eq!(msg, "backpressure");
}

#[test]
fn decode_structured_error_payload_rejects_non_exact_length() {
    // msg_len says 5, but only 4 bytes are present.
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.extend_from_slice(&5u16.to_be_bytes());
    payload.extend_from_slice(b"test");
    assert!(decode_structured_error_payload(&payload).is_none());

    // msg_len says 0, but extra bytes are present.
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.extend_from_slice(&0u16.to_be_bytes());
    payload.extend_from_slice(b"extra");
    assert!(decode_structured_error_payload(&payload).is_none());
}

#[test]
fn decode_structured_error_payload_rejects_invalid_utf8() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.push(0xFF);
    assert!(decode_structured_error_payload(&payload).is_none());
}

#[test]
fn decode_structured_error_payload_rejects_too_short_payloads() {
    assert!(decode_structured_error_payload(&[]).is_none());
    assert!(decode_structured_error_payload(&[0x00]).is_none());
    assert!(decode_structured_error_payload(&[0x00, 0x01, 0x02]).is_none());
}
