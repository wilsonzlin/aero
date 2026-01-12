use aero_l2_protocol::{
    decode_message, encode_with_limits, DecodeError, Limits, L2_TUNNEL_MAGIC, L2_TUNNEL_VERSION,
};
use serde::Deserialize;

const VECTORS_JSON: &str = include_str!("../../conformance/test-vectors/aero-vectors-v1.json");

#[derive(Debug, Deserialize)]
struct RootVectors {
    version: u32,
    #[serde(rename = "aero-l2-tunnel-v1")]
    l2: L2Vectors,
}

#[derive(Debug, Deserialize)]
struct L2Vectors {
    valid: Vec<L2ValidVector>,
    invalid: Vec<L2InvalidVector>,
}

#[derive(Debug, Deserialize)]
struct StructuredErrorVector {
    code: u16,
    message: String,
}

#[derive(Debug, Deserialize)]
struct L2ValidVector {
    name: String,
    #[serde(rename = "msgType")]
    msg_type: u8,
    flags: u8,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
    #[serde(default)]
    structured: Option<StructuredErrorVector>,
    #[serde(rename = "wireHex")]
    wire_hex: String,
}

#[derive(Debug, Deserialize)]
struct L2InvalidVector {
    name: String,
    #[serde(rename = "wireHex")]
    wire_hex: String,
    #[serde(rename = "errorCode")]
    error_code: String,
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert!(
        hex.len().is_multiple_of(2),
        "hex string must be an even number of chars, got {}",
        hex.len()
    );
    let mut out = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.as_bytes().iter().copied();
    while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
        let hi = from_hex(hi);
        let lo = from_hex(lo);
        out.push((hi << 4) | lo);
    }
    out
}

fn from_hex(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        other => panic!("invalid hex byte: {other:?}"),
    }
}

fn decode_error_code(err: &DecodeError) -> &'static str {
    match err {
        DecodeError::TooShort { .. } => "too_short",
        DecodeError::InvalidMagic { .. } => "invalid_magic",
        DecodeError::UnsupportedVersion { .. } => "unsupported_version",
        DecodeError::PayloadTooLarge { .. } => "payload_too_large",
    }
}

#[test]
fn l2_tunnel_vectors_roundtrip() {
    let vectors: RootVectors = serde_json::from_str(VECTORS_JSON).expect("parse vectors json");
    assert_eq!(vectors.version, 1, "unexpected vector file version");

    let limits = Limits::default();

    for vector in vectors.l2.valid {
        let payload = decode_hex(&vector.payload_hex);
        let wire = decode_hex(&vector.wire_hex);

        assert!(
            wire.len() >= aero_l2_protocol::L2_TUNNEL_HEADER_LEN,
            "{}: wire too short",
            vector.name
        );
        assert_eq!(wire[0], L2_TUNNEL_MAGIC, "{}", vector.name);
        assert_eq!(wire[1], L2_TUNNEL_VERSION, "{}", vector.name);
        assert_eq!(wire[2], vector.msg_type, "{}", vector.name);
        assert_eq!(wire[3], vector.flags, "{}", vector.name);
        assert_eq!(
            wire[aero_l2_protocol::L2_TUNNEL_HEADER_LEN..],
            payload,
            "{}",
            vector.name
        );

        if let Some(structured) = &vector.structured {
            let expected_payload = aero_l2_protocol::encode_structured_error_payload(
                structured.code,
                &structured.message,
                usize::MAX,
            );
            assert_eq!(expected_payload, payload, "{}", vector.name);
            let decoded = aero_l2_protocol::decode_structured_error_payload(&payload)
                .expect("expected structured error payload");
            assert_eq!(decoded.0, structured.code, "{}", vector.name);
            assert_eq!(decoded.1, structured.message, "{}", vector.name);
        }

        let decoded = decode_message(&wire).unwrap_or_else(|err| {
            panic!("decode failed for {}: {err:?}", vector.name);
        });

        assert_eq!(decoded.version, L2_TUNNEL_VERSION, "{}", vector.name);
        assert_eq!(decoded.msg_type, vector.msg_type, "{}", vector.name);
        assert_eq!(decoded.flags, vector.flags, "{}", vector.name);
        assert_eq!(decoded.payload, payload.as_slice(), "{}", vector.name);

        let encoded = encode_with_limits(vector.msg_type, vector.flags, &payload, &limits)
            .unwrap_or_else(|err| {
                panic!("encode failed for {}: {err:?}", vector.name);
            });
        assert_eq!(encoded, wire, "{}", vector.name);
    }
}

#[test]
fn l2_tunnel_vectors_invalid() {
    let vectors: RootVectors = serde_json::from_str(VECTORS_JSON).expect("parse vectors json");
    assert_eq!(vectors.version, 1, "unexpected vector file version");

    let limits = Limits::default();

    for vector in vectors.l2.invalid {
        let wire = decode_hex(&vector.wire_hex);
        let err = aero_l2_protocol::decode_with_limits(&wire, &limits).expect_err(&vector.name);
        assert_eq!(
            decode_error_code(&err),
            vector.error_code,
            "{}",
            vector.name
        );
    }
}
