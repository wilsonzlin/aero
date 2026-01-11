use std::path::PathBuf;

use aero_l2_protocol::{
    decode_message, encode_frame, encode_ping, encode_pong, encode_with_limits, Limits,
    L2_TUNNEL_TYPE_ERROR, L2_TUNNEL_TYPE_FRAME, L2_TUNNEL_TYPE_PING, L2_TUNNEL_TYPE_PONG,
    L2_TUNNEL_VERSION,
};
use base64::Engine as _;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct VectorsFile {
    schema: u32,
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    name: String,
    #[serde(rename = "msgType")]
    msg_type: Option<u8>,
    #[serde(rename = "type")]
    type_: Option<u8>,
    flags: Option<u8>,
    payload_b64: Option<String>,
    wire_b64: Option<String>,
    frame_b64: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol-vectors/l2-tunnel-v1.json")
}

fn decode_b64(b64: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("base64 decode")
}

#[test]
fn l2_tunnel_v1_vectors() {
    let raw = std::fs::read_to_string(vectors_path()).expect("read vectors file");
    let file: VectorsFile = serde_json::from_str(&raw).expect("parse vectors JSON");
    assert_eq!(file.schema, 1);

    for v in file.vectors {
        let wire_b64 = v
            .wire_b64
            .as_deref()
            .or(v.frame_b64.as_deref())
            .unwrap_or_else(|| panic!("vector {} missing wire_b64/frame_b64", v.name));
        let wire = decode_b64(wire_b64);

        if v.expect_error.unwrap_or(false) {
            let err =
                decode_message(&wire).expect_err(&format!("vector {} expected error", v.name));
            if let Some(substr) = v.error_contains {
                let msg = err.to_string();
                assert!(
                    msg.contains(&substr),
                    "vector {}: expected error to contain {:?}, got {:?}",
                    v.name,
                    substr,
                    msg
                );
            }
            continue;
        }

        let msg_type = v
            .msg_type
            .or(v.type_)
            .unwrap_or_else(|| panic!("vector {} missing msgType/type", v.name));
        let flags = v.flags.unwrap_or(0);
        let payload_b64 = v
            .payload_b64
            .as_deref()
            .unwrap_or_else(|| panic!("vector {} missing payload_b64", v.name));
        let payload = decode_b64(payload_b64);

        let decoded =
            decode_message(&wire).unwrap_or_else(|err| panic!("vector {} decode: {err}", v.name));
        assert_eq!(decoded.version, L2_TUNNEL_VERSION, "vector {}", v.name);
        assert_eq!(decoded.msg_type, msg_type, "vector {}", v.name);
        assert_eq!(decoded.flags, flags, "vector {}", v.name);
        assert_eq!(decoded.payload, payload.as_slice(), "vector {}", v.name);

        let encoded = match msg_type {
            L2_TUNNEL_TYPE_FRAME => encode_frame(&payload).expect("encode frame"),
            L2_TUNNEL_TYPE_PING => encode_ping(Some(&payload)).expect("encode ping"),
            L2_TUNNEL_TYPE_PONG => encode_pong(Some(&payload)).expect("encode pong"),
            L2_TUNNEL_TYPE_ERROR => {
                encode_with_limits(msg_type, flags, &payload, &Limits::default())
                    .expect("encode error")
            }
            _ => encode_with_limits(msg_type, flags, &payload, &Limits::default())
                .expect("encode message"),
        };
        assert_eq!(encoded, wire, "vector {}", v.name);
    }
}
