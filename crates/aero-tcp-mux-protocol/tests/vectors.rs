use aero_tcp_mux_protocol::{
    decode_close_payload, decode_error_payload, decode_frame, decode_open_payload,
    encode_close_payload, encode_error_payload, encode_frame, encode_frame_with_limits,
    encode_open_payload, FrameParser, Limits,
};
use base64::Engine;
use serde::Deserialize;

const VECTORS_JSON: &str = include_str!("../../../protocol-vectors/tcp-mux-v1.json");

#[derive(Debug, Deserialize)]
struct VectorFile {
    schema: u32,
    frames: Vec<FrameVector>,
    #[serde(rename = "openPayloads")]
    open_payloads: Vec<OpenPayloadVector>,
    #[serde(rename = "closePayloads")]
    close_payloads: Vec<ClosePayloadVector>,
    #[serde(rename = "errorPayloads")]
    error_payloads: Vec<ErrorPayloadVector>,
    #[serde(rename = "parserStreams")]
    parser_streams: Vec<ParserStreamVector>,
}

#[derive(Debug, Deserialize)]
struct FrameVector {
    name: String,
    #[serde(rename = "msgType")]
    msg_type: u8,
    #[serde(rename = "streamId")]
    stream_id: u32,
    payload_b64: String,
    frame_b64: String,
}

#[derive(Debug, Deserialize)]
struct OpenPayloadVector {
    name: String,
    host: String,
    port: u16,
    metadata: Option<String>,
    payload_b64: String,
}

#[derive(Debug, Deserialize)]
struct ClosePayloadVector {
    name: String,
    flags: u8,
    payload_b64: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ErrorPayloadVector {
    Ok {
        name: String,
        payload_b64: String,
        code: u16,
        message: String,
    },
    Err {
        name: String,
        payload_b64: String,
        #[serde(rename = "expectError")]
        _expect_error: bool,
        #[serde(rename = "errorContains")]
        error_contains: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ParserStreamVector {
    Ok {
        name: String,
        #[serde(rename = "chunks_b64")]
        chunks_b64: Vec<String>,
        #[serde(rename = "expectFrames")]
        expect_frames: Vec<ParserStreamFrame>,
    },
    Err {
        name: String,
        #[serde(rename = "chunks_b64")]
        chunks_b64: Vec<String>,
        #[serde(rename = "expectError")]
        _expect_error: bool,
        #[serde(rename = "errorContains")]
        error_contains: String,
    },
}

#[derive(Debug, Deserialize)]
struct ParserStreamFrame {
    #[serde(rename = "msgType")]
    msg_type: u8,
    #[serde(rename = "streamId")]
    stream_id: u32,
    payload_b64: String,
}

fn decode_b64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .expect("base64 decode")
}

#[test]
fn tcp_mux_protocol_vectors() {
    let vf: VectorFile = serde_json::from_str(VECTORS_JSON).expect("parse tcp-mux-v1.json");
    assert_eq!(vf.schema, 1);

    for v in vf.frames {
        let payload = decode_b64(&v.payload_b64);
        let expected_frame = decode_b64(&v.frame_b64);

        let decoded = decode_frame(&expected_frame)
            .unwrap_or_else(|e| panic!("decode frame/{}: {e}", v.name));
        assert_eq!(decoded.msg_type, v.msg_type, "frame/{}", v.name);
        assert_eq!(decoded.stream_id, v.stream_id, "frame/{}", v.name);
        assert_eq!(decoded.payload, payload, "frame/{}", v.name);

        let encoded = encode_frame(v.msg_type, v.stream_id, &payload)
            .unwrap_or_else(|e| panic!("encode frame/{}: {e}", v.name));
        assert_eq!(encoded, expected_frame, "frame/{}", v.name);
    }
}

#[test]
fn tcp_mux_open_close_error_payload_vectors() {
    let vf: VectorFile = serde_json::from_str(VECTORS_JSON).expect("parse tcp-mux-v1.json");
    assert_eq!(vf.schema, 1);

    for v in vf.open_payloads {
        let expected = decode_b64(&v.payload_b64);
        let encoded = encode_open_payload(&v.host, v.port, v.metadata.as_deref())
            .unwrap_or_else(|e| panic!("encode openPayload/{}: {e}", v.name));
        assert_eq!(encoded, expected, "openPayload/{}", v.name);

        let decoded = decode_open_payload(&encoded)
            .unwrap_or_else(|e| panic!("decode openPayload/{}: {e}", v.name));
        assert_eq!(decoded.host, v.host, "openPayload/{}", v.name);
        assert_eq!(decoded.port, v.port, "openPayload/{}", v.name);
        assert_eq!(decoded.metadata, v.metadata, "openPayload/{}", v.name);
    }

    for v in vf.close_payloads {
        let expected = decode_b64(&v.payload_b64);
        let encoded = encode_close_payload(v.flags);
        assert_eq!(encoded, expected, "closePayload/{}", v.name);

        let decoded = decode_close_payload(&encoded)
            .unwrap_or_else(|e| panic!("decode closePayload/{}: {e}", v.name));
        assert_eq!(decoded, v.flags, "closePayload/{}", v.name);
    }

    for v in vf.error_payloads {
        match v {
            ErrorPayloadVector::Ok {
                name,
                payload_b64,
                code,
                message,
            } => {
                let payload = decode_b64(&payload_b64);
                let encoded = encode_error_payload(code, &message)
                    .unwrap_or_else(|e| panic!("encode errorPayload/{name}: {e}"));
                assert_eq!(encoded, payload, "errorPayload/{name}");

                let decoded = decode_error_payload(&payload)
                    .unwrap_or_else(|e| panic!("decode errorPayload/{name}: {e}"));
                assert_eq!(decoded.code, code, "errorPayload/{name}");
                assert_eq!(decoded.message, message, "errorPayload/{name}");
            }
            ErrorPayloadVector::Err {
                name,
                payload_b64,
                error_contains,
                ..
            } => {
                let payload = decode_b64(&payload_b64);
                let err = decode_error_payload(&payload)
                    .unwrap_err_or_else(|| panic!("expected error for errorPayload/{name}"));
                assert!(
                    err.to_string().contains(&error_contains),
                    "errorPayload/{name}: want error containing {error_contains:?}, got {err}"
                );
            }
        }
    }
}

#[test]
fn tcp_mux_parser_stream_vectors() {
    let vf: VectorFile = serde_json::from_str(VECTORS_JSON).expect("parse tcp-mux-v1.json");
    assert_eq!(vf.schema, 1);

    for v in vf.parser_streams {
        match v {
            ParserStreamVector::Ok {
                name,
                chunks_b64,
                expect_frames,
            } => {
                let mut parser = FrameParser::new();
                let mut got = Vec::new();

                for chunk_b64 in chunks_b64 {
                    got.extend(
                        parser
                            .push(&decode_b64(&chunk_b64))
                            .unwrap_or_else(|e| panic!("parserStream/{name}: push error: {e}")),
                    );
                }

                parser
                    .finish()
                    .unwrap_or_else(|e| panic!("parserStream/{name}: finish error: {e}"));

                assert_eq!(got.len(), expect_frames.len(), "parserStream/{name}");
                for (i, expected) in expect_frames.iter().enumerate() {
                    let frame = &got[i];
                    assert_eq!(
                        frame.msg_type, expected.msg_type,
                        "parserStream/{name}[{i}]"
                    );
                    assert_eq!(
                        frame.stream_id, expected.stream_id,
                        "parserStream/{name}[{i}]"
                    );
                    assert_eq!(
                        frame.payload,
                        decode_b64(&expected.payload_b64),
                        "parserStream/{name}[{i}]"
                    );
                }
            }
            ParserStreamVector::Err {
                name,
                chunks_b64,
                error_contains,
                ..
            } => {
                let mut parser = FrameParser::new();
                for chunk_b64 in chunks_b64 {
                    // Truncated-stream errors are emitted from `finish()`; `push()` should not panic.
                    let _ = parser.push(&decode_b64(&chunk_b64)).unwrap();
                }

                let err = parser
                    .finish()
                    .unwrap_err_or_else(|| panic!("expected error for parserStream/{name}"));
                assert!(
                    err.to_string().contains(&error_contains),
                    "parserStream/{name}: want error containing {error_contains:?}, got {err}"
                );
            }
        }
    }
}

#[test]
fn parser_accepts_arbitrary_chunk_sizes() {
    let vf: VectorFile = serde_json::from_str(VECTORS_JSON).expect("parse tcp-mux-v1.json");
    assert_eq!(vf.schema, 1);

    // Concatenate all known-good frames and ensure the streaming parser yields the same sequence
    // regardless of input chunking.
    let mut wire = Vec::new();
    let mut expected = Vec::new();
    for v in &vf.frames {
        let frame_bytes = decode_b64(&v.frame_b64);
        wire.extend_from_slice(&frame_bytes);
        expected.push((v.msg_type, v.stream_id, decode_b64(&v.payload_b64)));
    }

    let mut parser = FrameParser::new();
    let mut got = Vec::new();

    // Deterministic pseudo-random chunking.
    let mut rng = 0x1234_5678u32;
    let mut offset = 0usize;
    while offset < wire.len() {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        let chunk_len = (rng % 17 + 1) as usize; // 1..=17
        let end = (offset + chunk_len).min(wire.len());
        got.extend(parser.push(&wire[offset..end]).unwrap());
        offset = end;
    }

    parser.finish().unwrap();

    assert_eq!(got.len(), expected.len());
    for (i, (msg_type, stream_id, payload)) in expected.into_iter().enumerate() {
        assert_eq!(got[i].msg_type, msg_type, "frame[{i}]");
        assert_eq!(got[i].stream_id, stream_id, "frame[{i}]");
        assert_eq!(got[i].payload, payload, "frame[{i}]");
    }
}

#[test]
fn enforces_max_payload_len() {
    let max = 8usize;
    let limits = Limits {
        max_payload_len: max,
    };

    let payload = vec![0u8; max + 1];
    let err = encode_frame_with_limits(1, 1, &payload, &limits)
        .unwrap_err_or_else(|| panic!("expected encode error"));
    assert!(err.to_string().contains("frame too large"));

    // Decoder should reject the header immediately without requiring the full payload.
    let mut hdr = Vec::new();
    hdr.push(1u8);
    hdr.extend_from_slice(&1u32.to_be_bytes());
    hdr.extend_from_slice(&((max as u32) + 1).to_be_bytes());
    let mut parser = FrameParser::with_limits(limits);
    let err = parser
        .push(&hdr)
        .unwrap_err_or_else(|| panic!("expected parse error"));
    assert!(err.to_string().contains("frame too large"));
}

trait UnwrapErrOrElse<T> {
    fn unwrap_err_or_else<F: FnOnce() -> T>(self, f: F) -> T;
}

impl<T, E> UnwrapErrOrElse<E> for Result<T, E> {
    fn unwrap_err_or_else<F: FnOnce() -> E>(self, f: F) -> E {
        match self {
            Ok(_) => f(),
            Err(e) => e,
        }
    }
}
