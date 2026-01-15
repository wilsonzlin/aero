#![cfg(not(target_arch = "wasm32"))]

use std::sync::OnceLock;

use aero_auth_tokens::{verify_gateway_session_token_checked, verify_hs256_jwt_checked};
use aero_l2_protocol::{
    decode_message as decode_l2, decode_structured_error_payload, encode_frame as encode_l2_frame,
    encode_ping as encode_l2_ping, encode_pong as encode_l2_pong, encode_with_limits, Limits,
    L2_TUNNEL_TYPE_ERROR, L2_TUNNEL_TYPE_FRAME, L2_TUNNEL_TYPE_PING, L2_TUNNEL_TYPE_PONG,
    L2_TUNNEL_VERSION,
};
use aero_tcp_mux_protocol::{
    decode_close_payload, decode_error_payload, decode_open_payload, encode_close_payload,
    encode_error_payload, encode_frame as encode_tcp_mux_frame, encode_open_payload, Frame,
    FrameParser,
};
use aero_udp_relay_protocol::{
    decode_datagram as decode_udp_relay_datagram, encode_datagram as encode_udp_relay_datagram,
    AddressFamily as UdpRelayAddressFamily, Datagram as UdpRelayDatagram,
    FramingVersion as UdpRelayFramingVersion, TransportFlags as UdpRelayTransportFlags,
};
use serde::Deserialize;

const VECTORS_V2_JSON: &str = include_str!("../test-vectors/aero-vectors-v2.json");

fn decode_hex(hex: &str) -> Vec<u8> {
    assert!(
        hex.len().is_multiple_of(2),
        "hex length must be even, got {}",
        hex.len()
    );
    let mut out = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16)
            .unwrap_or_else(|_| panic!("invalid hex at byte offset {i}"));
        out.push(byte);
    }
    out
}

#[derive(Debug, Deserialize)]
struct VectorsFileV2 {
    version: u32,
    #[serde(rename = "aero-l2-tunnel-v1")]
    l2: L2TunnelVectors,
    #[serde(rename = "aero-tcp-mux-v1")]
    tcp_mux: TcpMuxVectors,
    #[serde(rename = "aero-udp-relay-v1v2")]
    udp_relay: UdpRelayVectors,
    aero_session: SessionVectors,
    #[serde(rename = "aero-udp-relay-jwt-hs256")]
    udp_relay_jwt: JwtVectors,
}

#[derive(Debug, Deserialize)]
struct L2TunnelVectors {
    valid: Vec<L2ValidVector>,
    invalid: Vec<L2InvalidVector>,
}

#[derive(Debug, Deserialize)]
struct L2ValidVector {
    name: String,
    #[serde(rename = "msgType")]
    msg_type: u8,
    flags: u8,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
    #[serde(rename = "wireHex")]
    wire_hex: String,
    structured: Option<L2StructuredError>,
}

#[derive(Debug, Deserialize)]
struct L2StructuredError {
    code: u16,
    message: String,
}

#[derive(Debug, Deserialize)]
struct L2InvalidVector {
    name: String,
    #[serde(rename = "wireHex")]
    wire_hex: String,
    #[serde(rename = "errorCode")]
    error_code: String,
}

#[derive(Debug, Deserialize)]
struct TcpMuxVectors {
    schema: u32,
    frames: Vec<TcpMuxFrameVector>,
    #[serde(rename = "openPayloads")]
    open_payloads: Vec<TcpMuxOpenPayloadVector>,
    #[serde(rename = "closePayloads")]
    close_payloads: Vec<TcpMuxClosePayloadVector>,
    #[serde(rename = "errorPayloads")]
    error_payloads: Vec<TcpMuxErrorPayloadVector>,
    #[serde(rename = "parserStreams")]
    parser_streams: Vec<TcpMuxParserStreamVector>,
}

#[derive(Debug, Deserialize)]
struct TcpMuxFrameVector {
    name: String,
    #[serde(rename = "msgType")]
    msg_type: u8,
    #[serde(rename = "streamId")]
    stream_id: u32,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
    #[serde(rename = "frameHex")]
    frame_hex: String,
}

#[derive(Debug, Deserialize)]
struct TcpMuxOpenPayloadVector {
    name: String,
    host: String,
    port: u16,
    metadata: Option<String>,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
struct TcpMuxClosePayloadVector {
    name: String,
    flags: u8,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TcpMuxErrorPayloadVector {
    Ok {
        name: String,
        #[serde(rename = "payloadHex")]
        payload_hex: String,
        code: u16,
        message: String,
    },
    Err {
        name: String,
        #[serde(rename = "payloadHex")]
        payload_hex: String,
        #[serde(rename = "expectError")]
        expect_error: bool,
        #[serde(rename = "errorContains")]
        error_contains: String,
    },
}

#[derive(Debug, Deserialize)]
struct TcpMuxExpectedFrame {
    #[serde(rename = "msgType")]
    msg_type: u8,
    #[serde(rename = "streamId")]
    stream_id: u32,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TcpMuxParserStreamVector {
    Ok {
        name: String,
        #[serde(rename = "chunksHex")]
        chunks_hex: Vec<String>,
        #[serde(rename = "expectFrames")]
        expect_frames: Vec<TcpMuxExpectedFrame>,
    },
    Err {
        name: String,
        #[serde(rename = "chunksHex")]
        chunks_hex: Vec<String>,
        #[serde(rename = "expectError")]
        expect_error: bool,
        #[serde(rename = "errorContains")]
        error_contains: String,
    },
}

#[derive(Debug, Deserialize)]
struct UdpRelayVectors {
    schema: u32,
    vectors: Vec<UdpRelayVector>,
}

#[derive(Debug, Deserialize)]
struct UdpRelayVector {
    name: String,
    #[serde(rename = "frameHex")]
    frame_hex: String,

    // Valid vectors.
    version: Option<u8>,
    #[serde(rename = "guestPort")]
    guest_port: Option<u16>,
    #[serde(rename = "remoteIp")]
    remote_ip: Option<String>,
    #[serde(rename = "remotePort")]
    remote_port: Option<u16>,
    #[serde(rename = "payloadHex")]
    payload_hex: Option<String>,

    // Error vectors.
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionVectors {
    secret: String,
    #[serde(rename = "nowMs")]
    now_ms: u64,
    tokens: SessionTokenSet,
}

#[derive(Debug, Deserialize)]
struct SessionTokenSet {
    valid: SessionTokenVector,
    expired: SessionTokenVector,
    #[serde(rename = "badSignature")]
    bad_signature: SessionTokenVector,
}

#[derive(Debug, Deserialize)]
struct SessionTokenVector {
    token: String,
    claims: SessionClaims,
}

#[derive(Debug, Deserialize)]
struct SessionClaims {
    sid: String,
    exp: i64,
}

#[derive(Debug, Deserialize)]
struct JwtVectors {
    secret: String,
    #[serde(rename = "nowUnix")]
    now_unix: i64,
    tokens: JwtTokenSet,
}

#[derive(Debug, Deserialize)]
struct JwtTokenSet {
    valid: JwtTokenVector,
    expired: JwtTokenVector,
    #[serde(rename = "badSignature")]
    bad_signature: JwtTokenVector,
}

#[derive(Debug, Deserialize)]
struct JwtTokenVector {
    token: String,
    claims: JwtClaims,
}

#[derive(Debug, Deserialize)]
struct JwtClaims {
    sid: String,
    exp: i64,
    iat: i64,
    origin: Option<String>,
    aud: Option<String>,
    iss: Option<String>,
}

static VECTORS_V2: OnceLock<VectorsFileV2> = OnceLock::new();

fn vectors_v2() -> &'static VectorsFileV2 {
    VECTORS_V2.get_or_init(|| serde_json::from_str(VECTORS_V2_JSON).expect("parse v2 vectors"))
}

#[test]
fn instruction_conformance() {
    if !cfg!(all(target_arch = "x86_64", unix)) {
        eprintln!("skipping conformance tests on non-x86_64/unix host");
        return;
    }

    let report = conformance::run_from_env().expect("conformance run failed");
    assert_eq!(report.failures, 0, "conformance failures detected");
}

#[test]
fn aero_vectors_v2_l2_tunnel() {
    let vectors = vectors_v2();
    assert_eq!(vectors.version, 2);

    for v in &vectors.l2.valid {
        let payload = decode_hex(&v.payload_hex);
        let wire = decode_hex(&v.wire_hex);

        let decoded = decode_l2(&wire).unwrap_or_else(|err| panic!("{} decode: {err}", v.name));
        assert_eq!(decoded.version, L2_TUNNEL_VERSION, "{}", v.name);
        assert_eq!(decoded.msg_type, v.msg_type, "{}", v.name);
        assert_eq!(decoded.flags, v.flags, "{}", v.name);
        assert_eq!(decoded.payload, payload.as_slice(), "{}", v.name);

        let encoded = match v.msg_type {
            L2_TUNNEL_TYPE_FRAME => encode_l2_frame(&payload).expect("encode frame"),
            L2_TUNNEL_TYPE_PING => encode_l2_ping(Some(&payload)).expect("encode ping"),
            L2_TUNNEL_TYPE_PONG => encode_l2_pong(Some(&payload)).expect("encode pong"),
            L2_TUNNEL_TYPE_ERROR => {
                encode_with_limits(v.msg_type, v.flags, &payload, &Limits::default())
                    .expect("encode error")
            }
            other => panic!("{}: unsupported l2 msgType {}", v.name, other),
        };
        assert_eq!(encoded, wire, "{}", v.name);

        if let Some(structured) = &v.structured {
            let got = decode_structured_error_payload(&payload)
                .unwrap_or_else(|| panic!("{}: expected structured ERROR payload", v.name));
            assert_eq!(got.0, structured.code, "{}", v.name);
            assert_eq!(got.1, structured.message, "{}", v.name);
        }
    }

    for v in &vectors.l2.invalid {
        let wire = decode_hex(&v.wire_hex);
        let err = decode_l2(&wire).expect_err(&format!("{} expected decode error", v.name));
        let code = match err {
            aero_l2_protocol::DecodeError::TooShort { .. } => "too_short",
            aero_l2_protocol::DecodeError::InvalidMagic { .. } => "invalid_magic",
            aero_l2_protocol::DecodeError::UnsupportedVersion { .. } => "unsupported_version",
            aero_l2_protocol::DecodeError::PayloadTooLarge { .. } => "payload_too_large",
        };
        assert_eq!(code, v.error_code, "{}", v.name);
    }
}

#[test]
fn aero_vectors_v2_tcp_mux() {
    let vectors = vectors_v2();
    assert_eq!(vectors.version, 2);
    assert_eq!(vectors.tcp_mux.schema, 1);

    for v in &vectors.tcp_mux.frames {
        let payload = decode_hex(&v.payload_hex);
        let frame = decode_hex(&v.frame_hex);

        let mut parser = FrameParser::new();
        let parsed = parser
            .push(&frame)
            .unwrap_or_else(|err| panic!("{} parser.push: {err}", v.name));
        assert_eq!(parsed.len(), 1, "{}", v.name);
        assert_eq!(parsed[0].msg_type, v.msg_type, "{}", v.name);
        assert_eq!(parsed[0].stream_id, v.stream_id, "{}", v.name);
        assert_eq!(parsed[0].payload, payload, "{}", v.name);
        parser
            .finish()
            .unwrap_or_else(|err| panic!("{} parser.finish: {err}", v.name));

        let encoded = encode_tcp_mux_frame(v.msg_type, v.stream_id, &payload)
            .unwrap_or_else(|err| panic!("{} encode_frame: {err}", v.name));
        assert_eq!(encoded, frame, "{}", v.name);
    }

    for v in &vectors.tcp_mux.open_payloads {
        let expected = decode_hex(&v.payload_hex);
        let encoded = encode_open_payload(&v.host, v.port, v.metadata.as_deref())
            .unwrap_or_else(|err| panic!("{} encode_open_payload: {err}", v.name));
        assert_eq!(encoded, expected, "{}", v.name);

        let decoded =
            decode_open_payload(&expected).unwrap_or_else(|err| panic!("{} decode: {err}", v.name));
        assert_eq!(decoded.host, v.host, "{}", v.name);
        assert_eq!(decoded.port, v.port, "{}", v.name);
        assert_eq!(decoded.metadata, v.metadata, "{}", v.name);
    }

    for v in &vectors.tcp_mux.close_payloads {
        let expected = decode_hex(&v.payload_hex);
        let encoded = encode_close_payload(v.flags);
        assert_eq!(encoded, expected, "{}", v.name);
        let decoded = decode_close_payload(&expected)
            .unwrap_or_else(|err| panic!("{} decode_close_payload: {err}", v.name));
        assert_eq!(decoded, v.flags, "{}", v.name);
    }

    for v in &vectors.tcp_mux.error_payloads {
        match v {
            TcpMuxErrorPayloadVector::Ok {
                name,
                payload_hex,
                code,
                message,
            } => {
                let expected = decode_hex(payload_hex);
                let encoded = encode_error_payload(*code, message)
                    .unwrap_or_else(|err| panic!("{name} encode_error_payload: {err}"));
                assert_eq!(encoded, expected, "{name}");

                let got = decode_error_payload(&expected)
                    .unwrap_or_else(|err| panic!("{name} decode_error_payload: {err}"));
                assert_eq!(got.code, *code, "{name}");
                assert_eq!(got.message, *message, "{name}");
            }
            TcpMuxErrorPayloadVector::Err {
                name,
                payload_hex,
                expect_error,
                error_contains,
            } => {
                assert!(
                    *expect_error,
                    "{name}: expectError must be true for error vectors"
                );
                let payload = decode_hex(payload_hex);
                let err = decode_error_payload(&payload)
                    .expect_err(&format!("{name} expected decode_error_payload error"));
                assert!(
                    err.to_string().contains(error_contains),
                    "{name}: expected error to contain {error_contains:?}, got {err}"
                );
            }
        }
    }

    for v in &vectors.tcp_mux.parser_streams {
        match v {
            TcpMuxParserStreamVector::Ok {
                name,
                chunks_hex,
                expect_frames,
            } => {
                let mut parser = FrameParser::new();
                let mut got: Vec<Frame> = Vec::new();
                for chunk_hex in chunks_hex {
                    let chunk = decode_hex(chunk_hex);
                    got.extend(
                        parser
                            .push(&chunk)
                            .unwrap_or_else(|err| panic!("{name} parser.push: {err}")),
                    );
                }
                parser
                    .finish()
                    .unwrap_or_else(|err| panic!("{name} parser.finish: {err}"));

                assert_eq!(got.len(), expect_frames.len(), "{name}");
                for (idx, expected) in expect_frames.iter().enumerate() {
                    let exp_payload = decode_hex(&expected.payload_hex);
                    let got_frame = &got[idx];
                    assert_eq!(got_frame.msg_type, expected.msg_type, "{name} frame {idx}");
                    assert_eq!(
                        got_frame.stream_id, expected.stream_id,
                        "{name} frame {idx}"
                    );
                    assert_eq!(got_frame.payload, exp_payload, "{name} frame {idx}");
                }
            }
            TcpMuxParserStreamVector::Err {
                name,
                chunks_hex,
                expect_error,
                error_contains,
            } => {
                assert!(
                    *expect_error,
                    "{name}: expectError must be true for error vectors"
                );
                let mut parser = FrameParser::new();
                for chunk_hex in chunks_hex {
                    let chunk = decode_hex(chunk_hex);
                    let _ = parser
                        .push(&chunk)
                        .unwrap_or_else(|err| panic!("{name} parser.push: {err}"));
                }
                let err = parser
                    .finish()
                    .expect_err(&format!("{name} expected parser.finish error"));
                assert!(
                    err.to_string().contains(error_contains),
                    "{name}: expected error to contain {error_contains:?}, got {err}"
                );
            }
        }
    }
}

#[test]
fn aero_vectors_v2_udp_relay() {
    let vectors = vectors_v2();
    assert_eq!(vectors.version, 2);
    assert_eq!(vectors.udp_relay.schema, 1);

    for v in &vectors.udp_relay.vectors {
        let frame = decode_hex(&v.frame_hex);

        if v.expect_error.unwrap_or(false) {
            let err = decode_udp_relay_datagram(&frame)
                .expect_err(&format!("{} expected decode error", v.name));
            let needle = v
                .error_contains
                .as_deref()
                .unwrap_or("expected errorContains for error vector");
            assert!(
                err.to_string().contains(needle),
                "{}: expected error to contain {:?}, got {err}",
                v.name,
                needle
            );
            continue;
        }

        let version = v.version.expect("valid vectors must include version");
        let guest_port = v.guest_port.expect("valid vectors must include guestPort");
        let remote_port = v
            .remote_port
            .expect("valid vectors must include remotePort");
        let remote_ip: std::net::IpAddr = v
            .remote_ip
            .as_ref()
            .expect("valid vectors must include remoteIp")
            .parse()
            .expect("parse remoteIp");
        let payload = decode_hex(
            v.payload_hex
                .as_ref()
                .expect("valid vectors must include payloadHex"),
        );

        let expected_transport = match version {
            1 => {
                assert!(
                    matches!(remote_ip, std::net::IpAddr::V4(_)),
                    "{}: v1 vectors must use IPv4",
                    v.name
                );
                UdpRelayTransportFlags::v1_ipv4()
            }
            2 => match remote_ip {
                std::net::IpAddr::V4(_) => UdpRelayTransportFlags::v2_ipv4(),
                std::net::IpAddr::V6(_) => UdpRelayTransportFlags::v2_ipv6(),
            },
            other => panic!("{}: unsupported udp relay version {}", v.name, other),
        };

        let got = decode_udp_relay_datagram(&frame)
            .unwrap_or_else(|err| panic!("{} decode_datagram: {err}", v.name));
        assert_eq!(got.guest_port, guest_port, "{}", v.name);
        assert_eq!(got.remote_port, remote_port, "{}", v.name);
        assert_eq!(got.remote_ip, remote_ip, "{}", v.name);
        assert_eq!(got.payload, payload.as_slice(), "{}", v.name);
        assert_eq!(got.transport, expected_transport, "{}", v.name);

        // Also validate the individual transport flags so future enum refactors are caught.
        let expected_version = match version {
            1 => UdpRelayFramingVersion::V1,
            2 => UdpRelayFramingVersion::V2,
            other => panic!("{}: unsupported udp relay version {}", v.name, other),
        };
        assert_eq!(got.transport.version, expected_version, "{}", v.name);
        let expected_af = match remote_ip {
            std::net::IpAddr::V4(_) => UdpRelayAddressFamily::Ipv4,
            std::net::IpAddr::V6(_) => UdpRelayAddressFamily::Ipv6,
        };
        assert_eq!(got.transport.address_family, expected_af, "{}", v.name);

        let encoded = encode_udp_relay_datagram(UdpRelayDatagram {
            guest_port,
            remote_ip,
            remote_port,
            payload: payload.as_slice(),
            transport: expected_transport,
        })
        .unwrap_or_else(|err| panic!("{} encode_datagram: {err}", v.name));
        assert_eq!(encoded, frame, "{}", v.name);
    }
}

#[test]
fn aero_vectors_v2_auth_tokens() {
    let vectors = vectors_v2();
    assert_eq!(vectors.version, 2);

    let session_secret = vectors.aero_session.secret.as_bytes();
    let now_ms = vectors.aero_session.now_ms;
    let valid = verify_gateway_session_token_checked(
        &vectors.aero_session.tokens.valid.token,
        session_secret,
        now_ms,
    )
    .expect("expected valid session token to verify");
    assert_eq!(valid.sid, vectors.aero_session.tokens.valid.claims.sid);
    assert_eq!(valid.exp_unix, vectors.aero_session.tokens.valid.claims.exp);

    assert!(
        verify_gateway_session_token_checked(
            &vectors.aero_session.tokens.expired.token,
            session_secret,
            now_ms
        )
        .is_err(),
        "expected expired session token to be rejected"
    );
    assert!(
        verify_gateway_session_token_checked(
            &vectors.aero_session.tokens.bad_signature.token,
            session_secret,
            now_ms
        )
        .is_err(),
        "expected bad signature session token to be rejected"
    );

    let jwt_secret = vectors.udp_relay_jwt.secret.as_bytes();
    let now_unix = vectors.udp_relay_jwt.now_unix;
    let got = verify_hs256_jwt_checked(
        &vectors.udp_relay_jwt.tokens.valid.token,
        jwt_secret,
        now_unix,
    )
    .expect("expected valid jwt to verify");
    assert_eq!(got.sid, vectors.udp_relay_jwt.tokens.valid.claims.sid);
    assert_eq!(got.exp, vectors.udp_relay_jwt.tokens.valid.claims.exp);
    assert_eq!(got.iat, vectors.udp_relay_jwt.tokens.valid.claims.iat);
    assert_eq!(got.origin, vectors.udp_relay_jwt.tokens.valid.claims.origin);
    assert_eq!(got.aud, vectors.udp_relay_jwt.tokens.valid.claims.aud);
    assert_eq!(got.iss, vectors.udp_relay_jwt.tokens.valid.claims.iss);

    assert!(
        verify_hs256_jwt_checked(
            &vectors.udp_relay_jwt.tokens.expired.token,
            jwt_secret,
            now_unix
        )
        .is_err(),
        "expected expired jwt to be rejected"
    );
    assert!(
        verify_hs256_jwt_checked(
            &vectors.udp_relay_jwt.tokens.bad_signature.token,
            jwt_secret,
            now_unix
        )
        .is_err(),
        "expected bad signature jwt to be rejected"
    );
}
