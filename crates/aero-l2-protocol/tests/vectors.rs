use std::{fs, path::PathBuf};

use aero_l2_protocol::*;
use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_vectors() -> Value {
    let path = repo_root().join("protocol-vectors/l2-tunnel-v1.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path:?}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("parse {path:?}: {err}"))
}

fn b64_digit(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn b64_to_bytes(b64: &str) -> Vec<u8> {
    if b64.is_empty() {
        return Vec::new();
    }

    let bytes = b64.as_bytes();
    assert!(
        bytes.len().is_multiple_of(4),
        "base64 length must be a multiple of 4 (got {})",
        bytes.len()
    );

    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks_exact(4) {
        let mut vals = [0u8; 4];
        let mut pad = 0;

        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                pad += 1;
                vals[i] = 0;
                continue;
            }

            vals[i] = b64_digit(c)
                .unwrap_or_else(|| panic!("invalid base64 character 0x{c:02x} ('{}')", c as char));
        }

        match pad {
            0 => {}
            1 => assert_eq!(chunk[3], b'=', "invalid base64 padding"),
            2 => assert_eq!(&chunk[2..], b"==", "invalid base64 padding"),
            _ => panic!("invalid base64 padding length: {pad}"),
        }

        let n = (u32::from(vals[0]) << 18)
            | (u32::from(vals[1]) << 12)
            | (u32::from(vals[2]) << 6)
            | u32::from(vals[3]);

        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }

    out
}

fn json_get<'a>(obj: &'a Value, key: &str, ctx: &str) -> &'a Value {
    obj.as_object()
        .unwrap_or_else(|| panic!("{ctx} must be a JSON object"))
        .get(key)
        .unwrap_or_else(|| panic!("missing field {ctx}.{key}"))
}

fn json_u64(value: &Value, ctx: &str) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("{ctx} must be an integer (got {value:?})"))
}

fn json_u8(value: &Value, ctx: &str) -> u8 {
    let n = json_u64(value, ctx);
    u8::try_from(n).unwrap_or_else(|_| panic!("{ctx} must fit in u8 (got {n})"))
}

fn json_u16(value: &Value, ctx: &str) -> u16 {
    let n = json_u64(value, ctx);
    u16::try_from(n).unwrap_or_else(|_| panic!("{ctx} must fit in u16 (got {n})"))
}

fn json_str<'a>(value: &'a Value, ctx: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{ctx} must be a string (got {value:?})"))
}

#[test]
fn l2_tunnel_vectors_match_golden_bytes() {
    let vectors = load_vectors();
    assert_eq!(
        json_u64(json_get(&vectors, "schema", "root"), "root.schema"),
        1,
        "unexpected protocol vector schema version"
    );

    let magic = json_u8(json_get(&vectors, "magic", "root"), "root.magic");
    let version = json_u8(json_get(&vectors, "version", "root"), "root.version");
    let flags = json_u8(json_get(&vectors, "flags", "root"), "root.flags");

    assert_eq!(magic, L2_TUNNEL_MAGIC);
    assert_eq!(version, L2_TUNNEL_VERSION);
    assert_eq!(flags, 0);

    let vecs = json_get(&vectors, "vectors", "root")
        .as_array()
        .unwrap_or_else(|| panic!("root.vectors must be an array"));

    let limits = Limits::default();

    let mut saw_frame = false;
    let mut saw_ping = false;
    let mut saw_pong = false;
    let mut saw_error = false;
    let mut saw_invalid = false;

    for v in vecs {
        let name = json_str(json_get(v, "name", "vector"), "vector.name");
        let ctx = format!("vector[{name}]");

        let expect_error = v
            .as_object()
            .and_then(|obj| obj.get("expectError"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if expect_error {
            saw_invalid = true;

            let wire_b64 = json_str(json_get(v, "wire_b64", &ctx), &format!("{ctx}.wire_b64"));
            let wire = b64_to_bytes(wire_b64);
            if let Some(frame_b64) = v
                .as_object()
                .and_then(|obj| obj.get("frame_b64"))
                .and_then(Value::as_str)
            {
                let frame = b64_to_bytes(frame_b64);
                assert_eq!(wire, frame, "{name}: wire_b64 must equal frame_b64");
            }

            let error_contains = json_str(
                json_get(v, "errorContains", &ctx),
                &format!("{ctx}.errorContains"),
            );

            let err = match decode_with_limits(&wire, &limits) {
                Ok(_) => panic!("{name}: expected decode to fail"),
                Err(err) => err.to_string(),
            };
            assert!(
                err.contains(error_contains),
                "{name}: expected error to contain {error_contains:?}, got {err:?}"
            );
            continue;
        }

        let msg_type = json_u8(json_get(v, "type", &ctx), &format!("{ctx}.type"));
        let msg_flags = json_u8(json_get(v, "flags", &ctx), &format!("{ctx}.flags"));

        let payload_b64 = json_str(
            json_get(v, "payload_b64", &ctx),
            &format!("{ctx}.payload_b64"),
        );
        let wire_b64 = json_str(json_get(v, "wire_b64", &ctx), &format!("{ctx}.wire_b64"));
        let frame_b64 = json_str(json_get(v, "frame_b64", &ctx), &format!("{ctx}.frame_b64"));

        let payload = b64_to_bytes(payload_b64);
        let wire = b64_to_bytes(wire_b64);
        let frame = b64_to_bytes(frame_b64);
        assert_eq!(wire, frame, "{name}: wire_b64 must equal frame_b64");

        assert!(
            frame.len() >= L2_TUNNEL_HEADER_LEN,
            "{name}: frame too short for header"
        );
        assert_eq!(frame[0], magic, "{name}: magic");
        assert_eq!(frame[1], version, "{name}: version");
        assert_eq!(frame[2], msg_type, "{name}: type");
        assert_eq!(frame[3], msg_flags, "{name}: flags");
        assert_eq!(
            &frame[L2_TUNNEL_HEADER_LEN..],
            payload.as_slice(),
            "{name}: payload must match frame bytes"
        );

        let decoded = decode_with_limits(&frame, &limits).unwrap();
        assert_eq!(decoded.version, version);
        assert_eq!(decoded.msg_type, msg_type);
        assert_eq!(decoded.flags, msg_flags);
        assert_eq!(decoded.payload, payload.as_slice());

        let encoded = encode_with_limits(msg_type, msg_flags, &payload, &limits)
            .unwrap_or_else(|err| panic!("{name}: encode_with_limits failed unexpectedly: {err}"));
        assert_eq!(
            encoded, frame,
            "{name}: encode_with_limits must match golden bytes"
        );

        match msg_type {
            L2_TUNNEL_TYPE_FRAME => {
                saw_frame = true;
                let encoded = encode_frame(&payload).unwrap();
                assert_eq!(
                    encoded, frame,
                    "{name}: encode_frame must match golden bytes"
                );
            }
            L2_TUNNEL_TYPE_PING => {
                saw_ping = true;
                let encoded = encode_ping(Some(&payload)).unwrap();
                assert_eq!(
                    encoded, frame,
                    "{name}: encode_ping must match golden bytes"
                );
            }
            L2_TUNNEL_TYPE_PONG => {
                saw_pong = true;
                let encoded = encode_pong(Some(&payload)).unwrap();
                assert_eq!(
                    encoded, frame,
                    "{name}: encode_pong must match golden bytes"
                );
            }
            L2_TUNNEL_TYPE_ERROR => {
                saw_error = true;

                if v.get("code").is_some() && v.get("message").is_some() {
                    let code = json_u16(json_get(v, "code", &ctx), &format!("{ctx}.code"));
                    let message = json_str(json_get(v, "message", &ctx), &format!("{ctx}.message"));

                    let expected_payload =
                        encode_structured_error_payload(code, message, usize::MAX);
                    assert_eq!(
                        expected_payload, payload,
                        "{name}: ERROR payload must match structured encoding"
                    );

                    let decoded = decode_structured_error_payload(&payload)
                        .expect("{name}: expected structured ERROR payload");
                    assert_eq!(decoded.0, code, "{name}: structured ERROR code");
                    assert_eq!(decoded.1, message, "{name}: structured ERROR message");
                }
            }
            other => panic!("{name}: unexpected l2 tunnel message type 0x{other:02x}"),
        }
    }

    assert!(saw_frame, "missing FRAME vector");
    assert!(saw_ping, "missing PING vector");
    assert!(saw_pong, "missing PONG vector");
    assert!(saw_error, "missing ERROR vector");
    assert!(saw_invalid, "missing invalid/error vectors");
}
