#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct LegacyVectorsFile {
    schema: u32,
    magic: u8,
    version: u8,
    flags: u8,
    vectors: Vec<LegacyVector>,
}

#[derive(Debug, Deserialize)]
struct LegacyVector {
    name: String,
    #[serde(rename = "type")]
    msg_type: Option<u8>,
    flags: Option<u8>,
    payload_b64: Option<String>,
    frame_b64: Option<String>,
    wire_b64: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ConformanceVectorsFile {
    version: u32,
    #[serde(rename = "aero-l2-tunnel-v1")]
    aero_l2_tunnel_v1: ConformanceL2TunnelVectors,
    aero_session: ConformanceSessionVectors,
    #[serde(rename = "aero-udp-relay-jwt-hs256")]
    aero_udp_relay_jwt_hs256: ConformanceJwtVectors,
}

#[derive(Debug, Deserialize)]
struct ConformanceL2TunnelVectors {
    valid: Vec<ConformanceValidVector>,
    invalid: Vec<ConformanceInvalidVector>,
}

#[derive(Debug, Deserialize)]
struct ConformanceValidVector {
    name: String,
    #[serde(rename = "msgType")]
    msg_type: u8,
    flags: u8,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
    #[serde(rename = "wireHex")]
    wire_hex: String,
}

#[derive(Debug, Deserialize)]
struct ConformanceInvalidVector {
    name: String,
    #[serde(rename = "wireHex")]
    wire_hex: String,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionVectors {
    secret: String,
    #[serde(rename = "nowMs")]
    now_ms: u64,
    tokens: ConformanceSessionTokens,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionTokens {
    valid: ConformanceSessionToken,
    expired: ConformanceSessionToken,
    #[serde(rename = "badSignature")]
    bad_signature: ConformanceSessionToken,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionToken {
    token: String,
    claims: ConformanceSessionClaims,
}

#[derive(Debug, Deserialize)]
struct ConformanceSessionClaims {
    sid: String,
    exp: u64,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtVectors {
    secret: String,
    #[serde(rename = "nowUnix")]
    now_unix: u64,
    tokens: ConformanceJwtTokens,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtTokens {
    valid: ConformanceJwtToken,
    expired: ConformanceJwtToken,
    #[serde(rename = "badSignature")]
    bad_signature: ConformanceJwtToken,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtToken {
    token: String,
    claims: ConformanceJwtClaims,
}

#[derive(Debug, Deserialize)]
struct ConformanceJwtClaims {
    iat: u64,
    exp: u64,
    sid: String,
    origin: Option<String>,
    aud: Option<String>,
    iss: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProtocolAuthTokensFile {
    schema: u32,
    #[serde(rename = "sessionTokens")]
    session_tokens: ProtocolSessionTokens,
    #[serde(rename = "jwtTokens")]
    jwt_tokens: ProtocolJwtTokens,
}

#[derive(Debug, Deserialize)]
struct ProtocolSessionTokens {
    #[serde(rename = "testSecret")]
    test_secret: String,
    vectors: Vec<ProtocolSessionTokenVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolSessionTokenVector {
    name: String,
    secret: Option<String>,
    token: String,
    sid: Option<String>,
    exp: Option<u64>,
    #[serde(rename = "nowMs")]
    now_ms: Option<u64>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ProtocolJwtTokens {
    #[serde(rename = "testSecret")]
    test_secret: String,
    vectors: Vec<ProtocolJwtTokenVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolJwtTokenVector {
    name: String,
    secret: Option<String>,
    token: String,
    #[serde(rename = "nowSec")]
    now_sec: Option<u64>,
    sid: Option<String>,
    exp: Option<u64>,
    iat: Option<u64>,
    origin: Option<String>,
    aud: Option<String>,
    iss: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ConformanceVectorsFileV2 {
    version: u32,
    #[serde(rename = "aero-l2-tunnel-v1")]
    aero_l2_tunnel_v1: ConformanceL2TunnelVectors,
    aero_session: ConformanceSessionVectors,
    #[serde(rename = "aero-udp-relay-jwt-hs256")]
    aero_udp_relay_jwt_hs256: ConformanceJwtVectors,
    #[serde(rename = "aero-tcp-mux-v1")]
    aero_tcp_mux_v1: ConformanceTcpMuxVectors,
    #[serde(rename = "aero-udp-relay-v1v2")]
    aero_udp_relay_v1v2: ConformanceUdpRelayVectors,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxVectors {
    schema: u32,
    frames: Vec<ConformanceTcpMuxFrameVector>,
    #[serde(rename = "openPayloads")]
    open_payloads: Vec<ConformanceTcpMuxOpenPayloadVector>,
    #[serde(rename = "closePayloads")]
    close_payloads: Vec<ConformanceTcpMuxClosePayloadVector>,
    #[serde(rename = "errorPayloads")]
    error_payloads: Vec<ConformanceTcpMuxErrorPayloadVector>,
    #[serde(rename = "parserStreams")]
    parser_streams: Vec<ConformanceTcpMuxParserStreamVector>,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxFrameVector {
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
struct ConformanceTcpMuxOpenPayloadVector {
    name: String,
    host: String,
    port: u16,
    metadata: Option<String>,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxClosePayloadVector {
    name: String,
    flags: u8,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxErrorPayloadVector {
    name: String,
    code: Option<u16>,
    message: Option<String>,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxParserStreamVector {
    name: String,
    #[serde(rename = "chunksHex")]
    chunks_hex: Vec<String>,
    #[serde(rename = "expectFrames")]
    expect_frames: Option<Vec<ConformanceTcpMuxExpectedFrame>>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxExpectedFrame {
    #[serde(rename = "msgType")]
    msg_type: u8,
    #[serde(rename = "streamId")]
    stream_id: u32,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
struct ConformanceUdpRelayVectors {
    schema: u32,
    vectors: Vec<ConformanceUdpRelayVector>,
}

#[derive(Debug, Deserialize)]
struct ConformanceUdpRelayVector {
    name: String,
    #[serde(rename = "frameHex")]
    frame_hex: String,
    version: Option<u8>,
    #[serde(rename = "guestPort")]
    guest_port: Option<u16>,
    #[serde(rename = "remoteIp")]
    remote_ip: Option<String>,
    #[serde(rename = "remotePort")]
    remote_port: Option<u16>,
    #[serde(rename = "payloadHex")]
    payload_hex: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxVectorsFile {
    schema: u32,
    frames: Vec<ProtocolTcpMuxFrameVector>,
    #[serde(rename = "openPayloads")]
    open_payloads: Vec<ProtocolTcpMuxOpenPayloadVector>,
    #[serde(rename = "closePayloads")]
    close_payloads: Vec<ProtocolTcpMuxClosePayloadVector>,
    #[serde(rename = "errorPayloads")]
    error_payloads: Vec<ProtocolTcpMuxErrorPayloadVector>,
    #[serde(rename = "parserStreams")]
    parser_streams: Vec<ProtocolTcpMuxParserStreamVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxFrameVector {
    name: String,
    #[serde(rename = "msgType")]
    msg_type: u8,
    #[serde(rename = "streamId")]
    stream_id: u32,
    payload_b64: String,
    frame_b64: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxOpenPayloadVector {
    name: String,
    host: String,
    port: u16,
    metadata: Option<String>,
    payload_b64: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxClosePayloadVector {
    name: String,
    flags: u8,
    payload_b64: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxErrorPayloadVector {
    name: String,
    code: Option<u16>,
    message: Option<String>,
    payload_b64: String,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxParserStreamVector {
    name: String,
    #[serde(rename = "chunks_b64")]
    chunks_b64: Vec<String>,
    #[serde(rename = "expectFrames")]
    expect_frames: Option<Vec<ProtocolTcpMuxExpectedFrame>>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxExpectedFrame {
    #[serde(rename = "msgType")]
    msg_type: u8,
    #[serde(rename = "streamId")]
    stream_id: u32,
    payload_b64: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolUdpRelayVectorsFile {
    schema: u32,
    vectors: Vec<ProtocolUdpRelayVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolUdpRelayVector {
    name: String,
    version: Option<u8>,
    frame_b64: String,
    #[serde(rename = "guestPort")]
    guest_port: Option<u16>,
    #[serde(rename = "remoteIp")]
    remote_ip: Option<String>,
    #[serde(rename = "remotePort")]
    remote_port: Option<u16>,
    payload_b64: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn legacy_vectors_path() -> PathBuf {
    repo_root().join("protocol-vectors/l2-tunnel-v1.json")
}

fn protocol_auth_tokens_path() -> PathBuf {
    repo_root().join("protocol-vectors/auth-tokens.json")
}

fn protocol_tcp_mux_v1_path() -> PathBuf {
    repo_root().join("protocol-vectors/tcp-mux-v1.json")
}

fn protocol_udp_relay_path() -> PathBuf {
    repo_root().join("protocol-vectors/udp-relay.json")
}

fn conformance_vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-vectors/aero-vectors-v1.json")
}

fn conformance_vectors_v2_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-vectors/aero-vectors-v2.json")
}

fn decode_b64(ctx: &str, b64: &str) -> Vec<u8> {
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

    if b64.is_empty() {
        return Vec::new();
    }

    let bytes = b64.as_bytes();
    assert!(
        bytes.len().is_multiple_of(4),
        "{ctx}: base64 length must be a multiple of 4 (got {})",
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

            vals[i] = b64_digit(c).unwrap_or_else(|| {
                panic!(
                    "{ctx}: invalid base64 character 0x{c:02x} ('{}')",
                    c as char
                )
            });
        }

        match pad {
            0 => {}
            1 => assert_eq!(chunk[3], b'=', "{ctx}: invalid base64 padding"),
            2 => assert_eq!(&chunk[2..], b"==", "{ctx}: invalid base64 padding"),
            _ => panic!("{ctx}: invalid base64 padding length: {pad}"),
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

fn decode_hex(ctx: &str, hex: &str) -> Vec<u8> {
    fn nibble(ctx: &str, b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => panic!("{ctx}: invalid hex digit 0x{b:02x} ('{}')", b as char),
        }
    }

    let bytes = hex.as_bytes();
    assert!(
        bytes.len().is_multiple_of(2),
        "{ctx}: hex length must be even (got {})",
        bytes.len()
    );
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for i in (0..bytes.len()).step_by(2) {
        out.push((nibble(ctx, bytes[i]) << 4) | nibble(ctx, bytes[i + 1]));
    }
    out
}

fn fmt_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i != 0 {
            out.push(' ');
        }
        write!(&mut out, "{b:02x}").expect("write! to String");
    }
    out
}

fn assert_bytes_eq(ctx: &str, expected: &[u8], actual: &[u8]) {
    if expected == actual {
        return;
    }

    let min = expected.len().min(actual.len());
    let mut i = 0;
    while i < min && expected[i] == actual[i] {
        i += 1;
    }

    if i == min && expected.len() != actual.len() {
        panic!(
            "{ctx}: length mismatch after {} matching bytes (expected {} bytes, got {} bytes)",
            i,
            expected.len(),
            actual.len()
        );
    }

    // i < min: we found the first mismatching byte.
    let exp = expected[i];
    let act = actual[i];

    let window = 16usize;
    let start = i.saturating_sub(window);
    let end = (i + window).min(expected.len().max(actual.len()));

    let expected_end = expected.len().min(end);
    let actual_end = actual.len().min(end);

    panic!(
        "{ctx}: first mismatch at byte {i} (expected 0x{exp:02x}, got 0x{act:02x})\n\
expected[{start}..{expected_end}): {}\n\
actual  [{start}..{actual_end}): {}",
        fmt_hex(&expected[start..expected_end]),
        fmt_hex(&actual[start..actual_end]),
    );
}

fn fmt_ascii(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for &b in bytes {
        match b {
            b' '..=b'~' => out.push(b as char),
            _ => write!(&mut out, "\\x{b:02x}").expect("write! to String"),
        }
    }
    out
}

fn assert_ascii_eq(ctx: &str, expected: &str, actual: &str) {
    if expected == actual {
        return;
    }

    let expected = expected.as_bytes();
    let actual = actual.as_bytes();

    let min = expected.len().min(actual.len());
    let mut i = 0;
    while i < min && expected[i] == actual[i] {
        i += 1;
    }

    if i == min && expected.len() != actual.len() {
        panic!(
            "{ctx}: length mismatch after {} matching bytes (expected {} bytes, got {} bytes)",
            i,
            expected.len(),
            actual.len()
        );
    }

    let exp = expected[i];
    let act = actual[i];

    let window = 32usize;
    let start = i.saturating_sub(window);
    let end = (i + window).min(expected.len().max(actual.len()));

    let expected_end = expected.len().min(end);
    let actual_end = actual.len().min(end);

    panic!(
        "{ctx}: first mismatch at byte {i} (expected 0x{exp:02x}, got 0x{act:02x})\n\
expected[{start}..{expected_end}): {}\n\
actual  [{start}..{actual_end}): {}",
        fmt_ascii(&expected[start..expected_end]),
        fmt_ascii(&actual[start..actual_end]),
    );
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    let raw = fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path:?}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("parse {path:?}: {err}"))
}

fn assert_l2_tunnel_vectors_match(
    legacy_path: &Path,
    legacy: &LegacyVectorsFile,
    conf: &ConformanceL2TunnelVectors,
    conf_label: &str,
) {
    let mut legacy_by_name: BTreeMap<&str, &LegacyVector> = BTreeMap::new();
    for v in &legacy.vectors {
        if let (Some(wire_b64), Some(frame_b64)) = (v.wire_b64.as_deref(), v.frame_b64.as_deref()) {
            let ctx = format!(
                "legacy l2-tunnel vector {:?}: wire_b64 vs frame_b64",
                v.name
            );
            let wire = decode_b64(&format!("{ctx}.wire_b64"), wire_b64);
            let frame = decode_b64(&format!("{ctx}.frame_b64"), frame_b64);
            assert_bytes_eq(&ctx, &wire, &frame);
        }

        if legacy_by_name.insert(v.name.as_str(), v).is_some() {
            panic!("duplicate legacy vector name: {}", v.name);
        }
    }

    // Compare all canonical conformance vectors to their legacy counterparts.
    // We intentionally only require the canonical ones to exist in the legacy file; the legacy
    // file may carry extra vectors for older consumers / additional coverage.
    for v in &conf.valid {
        let ctx = format!(
            "l2-tunnel valid vector {:?} (protocol-vectors vs {conf_label})",
            v.name
        );
        let legacy = legacy_by_name
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| {
                panic!(
                "{ctx}: missing legacy vector. Expected to find a matching entry in {legacy_path:?}"
            )
            });
        assert!(
            !legacy.expect_error.unwrap_or(false),
            "{ctx}: expected legacy vector to be valid (expectError=false)"
        );

        let legacy_type = legacy
            .msg_type
            .unwrap_or_else(|| panic!("{ctx}: legacy vector missing field `type`"));
        assert_eq!(
            legacy_type, v.msg_type,
            "{ctx}: msgType/type mismatch (conformance={}, legacy={legacy_type})",
            v.msg_type
        );

        let legacy_flags = legacy.flags.unwrap_or(0);
        assert_eq!(
            legacy_flags, v.flags,
            "{ctx}: flags mismatch (conformance={}, legacy={legacy_flags})",
            v.flags
        );

        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let legacy_payload = decode_b64(
            &format!("{ctx}: payload_b64"),
            legacy
                .payload_b64
                .as_deref()
                .unwrap_or_else(|| panic!("{ctx}: legacy vector missing field `payload_b64`")),
        );
        assert_bytes_eq(
            &format!("{ctx}: payload bytes"),
            &conf_payload,
            &legacy_payload,
        );

        let conf_wire = decode_hex(&format!("{ctx}: wireHex"), &v.wire_hex);
        let legacy_wire_b64 = legacy
            .wire_b64
            .as_deref()
            .or(legacy.frame_b64.as_deref())
            .unwrap_or_else(|| panic!("{ctx}: legacy vector missing `wire_b64`/`frame_b64`"));
        let legacy_wire = decode_b64(&format!("{ctx}: wire_b64"), legacy_wire_b64);
        assert_bytes_eq(&format!("{ctx}: wire bytes"), &conf_wire, &legacy_wire);
    }

    for v in &conf.invalid {
        let ctx = format!(
            "l2-tunnel invalid vector {:?} (protocol-vectors vs {conf_label})",
            v.name
        );
        let legacy = legacy_by_name
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| {
                panic!(
                "{ctx}: missing legacy vector. Expected to find a matching entry in {legacy_path:?}"
            )
            });
        assert!(
            legacy.expect_error.unwrap_or(false),
            "{ctx}: expected legacy vector to be invalid (expectError=true)"
        );

        let conf_wire = decode_hex(&format!("{ctx}: wireHex"), &v.wire_hex);
        let legacy_wire_b64 = legacy
            .wire_b64
            .as_deref()
            .or(legacy.frame_b64.as_deref())
            .unwrap_or_else(|| panic!("{ctx}: legacy vector missing `wire_b64`/`frame_b64`"));
        let legacy_wire = decode_b64(&format!("{ctx}: wire_b64"), legacy_wire_b64);
        assert_bytes_eq(&format!("{ctx}: wire bytes"), &conf_wire, &legacy_wire);
    }
}

fn assert_auth_token_vectors_match(
    proto_path: &Path,
    proto: &ProtocolAuthTokensFile,
    conf_session: &ConformanceSessionVectors,
    conf_jwt: &ConformanceJwtVectors,
    conf_label: &str,
) {
    // Session cookie (`aero_session`) vectors.
    assert_eq!(
        proto.session_tokens.test_secret, conf_session.secret,
        "session token secret drift (protocol-vectors vs {})",
        conf_label
    );

    let mut proto_session_by_name: BTreeMap<&str, &ProtocolSessionTokenVector> = BTreeMap::new();
    for v in &proto.session_tokens.vectors {
        let name = v.name.as_str();
        if proto_session_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol session token vector name: {name}");
        }
    }

    let session_cases = [
        ("valid", &conf_session.tokens.valid),
        ("expired", &conf_session.tokens.expired),
        ("badSignature", &conf_session.tokens.bad_signature),
    ];

    for (name, conf_token) in session_cases {
        let ctx = format!("session token vector {name:?} (protocol-vectors vs {conf_label})");
        let proto_v = proto_session_by_name.get(name).copied().unwrap_or_else(|| {
            panic!("{ctx}: missing entry in {proto_path:?} (sessionTokens.vectors)")
        });

        if let Some(secret) = &proto_v.secret {
            assert_eq!(secret, &conf_session.secret, "{ctx}: secret drift");
        }
        if let Some(now_ms) = proto_v.now_ms {
            assert_eq!(now_ms, conf_session.now_ms, "{ctx}: nowMs drift");
        }

        assert_ascii_eq(
            &format!("{ctx}: token drift"),
            &conf_token.token,
            &proto_v.token,
        );

        if name == "valid" {
            assert_eq!(
                proto_v.sid.as_deref(),
                Some(conf_token.claims.sid.as_str()),
                "{ctx}: sid claim drift"
            );
            assert_eq!(
                proto_v.exp,
                Some(conf_token.claims.exp),
                "{ctx}: exp claim drift"
            );
        } else {
            assert!(
                proto_v.expect_error.unwrap_or(false),
                "{ctx}: expected protocol-vectors to mark this case as expectError=true"
            );
        }
    }

    // UDP relay JWT vectors.
    assert_eq!(
        proto.jwt_tokens.test_secret, conf_jwt.secret,
        "jwt secret drift (protocol-vectors vs {})",
        conf_label
    );

    let mut proto_jwt_by_name: BTreeMap<&str, &ProtocolJwtTokenVector> = BTreeMap::new();
    for v in &proto.jwt_tokens.vectors {
        let name = v.name.as_str();
        if proto_jwt_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol jwt token vector name: {name}");
        }
    }

    let jwt_cases = [
        ("valid", &conf_jwt.tokens.valid),
        ("expired", &conf_jwt.tokens.expired),
        ("badSignature", &conf_jwt.tokens.bad_signature),
    ];

    for (name, conf_token) in jwt_cases {
        let ctx = format!("jwt vector {name:?} (protocol-vectors vs {conf_label})");
        let proto_v = proto_jwt_by_name.get(name).copied().unwrap_or_else(|| {
            panic!("{ctx}: missing entry in {proto_path:?} (jwtTokens.vectors)")
        });

        if let Some(secret) = &proto_v.secret {
            assert_eq!(secret, &conf_jwt.secret, "{ctx}: secret drift");
        }
        if let Some(now_sec) = proto_v.now_sec {
            assert_eq!(now_sec, conf_jwt.now_unix, "{ctx}: nowUnix drift");
        }

        assert_ascii_eq(
            &format!("{ctx}: token drift"),
            &conf_token.token,
            &proto_v.token,
        );

        if name == "valid" {
            let claims = &conf_token.claims;
            assert_eq!(proto_v.iat, Some(claims.iat), "{ctx}: iat claim drift");
            assert_eq!(proto_v.exp, Some(claims.exp), "{ctx}: exp claim drift");
            assert_eq!(
                proto_v.sid.as_deref(),
                Some(claims.sid.as_str()),
                "{ctx}: sid claim drift"
            );
            assert_eq!(
                proto_v.origin.as_deref(),
                claims.origin.as_deref(),
                "{ctx}: origin claim drift"
            );
            assert_eq!(
                proto_v.aud.as_deref(),
                claims.aud.as_deref(),
                "{ctx}: aud claim drift"
            );
            assert_eq!(
                proto_v.iss.as_deref(),
                claims.iss.as_deref(),
                "{ctx}: iss claim drift"
            );
        } else {
            assert!(
                proto_v.expect_error.unwrap_or(false),
                "{ctx}: expected protocol-vectors to mark this case as expectError=true"
            );
        }
    }
}

/// Prevent accidental protocol drift between the older `protocol-vectors/` set (base64) and the
/// canonical `crates/conformance/test-vectors/` set (hex).
#[test]
fn l2_tunnel_vectors_do_not_drift() {
    let legacy_path = legacy_vectors_path();
    let conf_path = conformance_vectors_path();

    let legacy: LegacyVectorsFile = read_json(&legacy_path);
    assert_eq!(legacy.schema, 1, "unexpected legacy vector schema version");
    assert_eq!(legacy.magic, 0xA2, "unexpected legacy l2 tunnel magic");
    assert_eq!(legacy.version, 0x03, "unexpected legacy l2 tunnel version");
    assert_eq!(legacy.flags, 0, "unexpected legacy l2 tunnel flags");

    let conf: ConformanceVectorsFile = read_json(&conf_path);
    assert_eq!(
        conf.version, 1,
        "unexpected conformance vector schema version"
    );
    assert_l2_tunnel_vectors_match(
        &legacy_path,
        &legacy,
        &conf.aero_l2_tunnel_v1,
        "aero-vectors-v1.json",
    );
}

#[test]
fn auth_token_vectors_do_not_drift() {
    let proto_path = protocol_auth_tokens_path();
    let conf_path = conformance_vectors_path();

    let proto: ProtocolAuthTokensFile = read_json(&proto_path);
    assert_eq!(
        proto.schema, 1,
        "unexpected protocol auth token vector schema"
    );

    let conf: ConformanceVectorsFile = read_json(&conf_path);
    assert_eq!(
        conf.version, 1,
        "unexpected conformance vector schema version"
    );
    assert_auth_token_vectors_match(
        &proto_path,
        &proto,
        &conf.aero_session,
        &conf.aero_udp_relay_jwt_hs256,
        "aero-vectors-v1.json",
    );
}

#[test]
fn l2_tunnel_vectors_do_not_drift_v2() {
    let legacy_path = legacy_vectors_path();
    let conf_path = conformance_vectors_v2_path();

    let legacy: LegacyVectorsFile = read_json(&legacy_path);
    assert_eq!(legacy.schema, 1, "unexpected legacy vector schema version");
    assert_eq!(legacy.magic, 0xA2, "unexpected legacy l2 tunnel magic");
    assert_eq!(legacy.version, 0x03, "unexpected legacy l2 tunnel version");
    assert_eq!(legacy.flags, 0, "unexpected legacy l2 tunnel flags");

    let conf: ConformanceVectorsFileV2 = read_json(&conf_path);
    assert_eq!(
        conf.version, 2,
        "unexpected conformance vector schema version"
    );
    assert_l2_tunnel_vectors_match(
        &legacy_path,
        &legacy,
        &conf.aero_l2_tunnel_v1,
        "aero-vectors-v2.json",
    );
}

#[test]
fn auth_token_vectors_do_not_drift_v2() {
    let proto_path = protocol_auth_tokens_path();
    let conf_path = conformance_vectors_v2_path();

    let proto: ProtocolAuthTokensFile = read_json(&proto_path);
    assert_eq!(
        proto.schema, 1,
        "unexpected protocol auth token vector schema"
    );

    let conf: ConformanceVectorsFileV2 = read_json(&conf_path);
    assert_eq!(
        conf.version, 2,
        "unexpected conformance vector schema version"
    );
    assert_auth_token_vectors_match(
        &proto_path,
        &proto,
        &conf.aero_session,
        &conf.aero_udp_relay_jwt_hs256,
        "aero-vectors-v2.json",
    );
}

#[test]
fn tcp_mux_vectors_do_not_drift() {
    let proto_path = protocol_tcp_mux_v1_path();
    let conf_path = conformance_vectors_v2_path();

    let proto: ProtocolTcpMuxVectorsFile = read_json(&proto_path);
    assert_eq!(proto.schema, 1, "unexpected protocol tcp-mux vector schema");

    let conf: ConformanceVectorsFileV2 = read_json(&conf_path);
    assert_eq!(
        conf.version, 2,
        "unexpected conformance vector schema version"
    );
    assert_eq!(
        conf.aero_tcp_mux_v1.schema, 1,
        "unexpected conformance tcp-mux schema"
    );

    let mut proto_frames: BTreeMap<&str, &ProtocolTcpMuxFrameVector> = BTreeMap::new();
    for v in &proto.frames {
        let name = v.name.as_str();
        if proto_frames.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux frame vector name: {name}");
        }
    }

    for v in &conf.aero_tcp_mux_v1.frames {
        let ctx = format!(
            "tcp-mux frame {:?} (protocol-vectors vs conformance)",
            v.name
        );
        let proto_v = proto_frames
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (frames)"));

        assert_eq!(proto_v.msg_type, v.msg_type, "{ctx}: msgType drift");
        assert_eq!(proto_v.stream_id, v.stream_id, "{ctx}: streamId drift");

        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(
            &format!("{ctx}: payload bytes"),
            &conf_payload,
            &proto_payload,
        );

        let conf_frame = decode_hex(&format!("{ctx}: frameHex"), &v.frame_hex);
        let proto_frame = decode_b64(&format!("{ctx}: frame_b64"), &proto_v.frame_b64);
        assert_bytes_eq(&format!("{ctx}: frame bytes"), &conf_frame, &proto_frame);
    }

    let mut proto_open: BTreeMap<&str, &ProtocolTcpMuxOpenPayloadVector> = BTreeMap::new();
    for v in &proto.open_payloads {
        let name = v.name.as_str();
        if proto_open.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux open payload vector name: {name}");
        }
    }
    for v in &conf.aero_tcp_mux_v1.open_payloads {
        let ctx = format!(
            "tcp-mux open payload {:?} (protocol-vectors vs conformance)",
            v.name
        );
        let proto_v = proto_open
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (openPayloads)"));
        assert_eq!(proto_v.host, v.host, "{ctx}: host drift");
        assert_eq!(proto_v.port, v.port, "{ctx}: port drift");
        assert_eq!(proto_v.metadata, v.metadata, "{ctx}: metadata drift");
        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(
            &format!("{ctx}: payload bytes"),
            &conf_payload,
            &proto_payload,
        );
    }

    let mut proto_close: BTreeMap<&str, &ProtocolTcpMuxClosePayloadVector> = BTreeMap::new();
    for v in &proto.close_payloads {
        let name = v.name.as_str();
        if proto_close.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux close payload vector name: {name}");
        }
    }
    for v in &conf.aero_tcp_mux_v1.close_payloads {
        let ctx = format!(
            "tcp-mux close payload {:?} (protocol-vectors vs conformance)",
            v.name
        );
        let proto_v = proto_close
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (closePayloads)"));
        assert_eq!(proto_v.flags, v.flags, "{ctx}: flags drift");
        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(
            &format!("{ctx}: payload bytes"),
            &conf_payload,
            &proto_payload,
        );
    }

    let mut proto_error: BTreeMap<&str, &ProtocolTcpMuxErrorPayloadVector> = BTreeMap::new();
    for v in &proto.error_payloads {
        let name = v.name.as_str();
        if proto_error.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux error payload vector name: {name}");
        }
    }
    for v in &conf.aero_tcp_mux_v1.error_payloads {
        let ctx = format!(
            "tcp-mux error payload {:?} (protocol-vectors vs conformance)",
            v.name
        );
        let proto_v = proto_error
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (errorPayloads)"));
        let proto_is_error = proto_v.expect_error.unwrap_or(false);
        let conf_is_error = v.expect_error.unwrap_or(false);
        assert_eq!(proto_is_error, conf_is_error, "{ctx}: expectError drift");
        if proto_is_error {
            assert_eq!(
                proto_v.error_contains.as_deref(),
                v.error_contains.as_deref(),
                "{ctx}: errorContains drift"
            );
        } else {
            assert_eq!(proto_v.code, v.code, "{ctx}: code drift");
            assert_eq!(proto_v.message, v.message, "{ctx}: message drift");
        }
        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(
            &format!("{ctx}: payload bytes"),
            &conf_payload,
            &proto_payload,
        );
    }

    let mut proto_streams: BTreeMap<&str, &ProtocolTcpMuxParserStreamVector> = BTreeMap::new();
    for v in &proto.parser_streams {
        let name = v.name.as_str();
        if proto_streams.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux parser stream vector name: {name}");
        }
    }
    for v in &conf.aero_tcp_mux_v1.parser_streams {
        let ctx = format!(
            "tcp-mux parser stream {:?} (protocol-vectors vs conformance)",
            v.name
        );
        let proto_v = proto_streams
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (parserStreams)"));

        assert_eq!(
            proto_v.chunks_b64.len(),
            v.chunks_hex.len(),
            "{ctx}: chunk count drift"
        );
        for (i, (proto_chunk_b64, conf_chunk_hex)) in
            proto_v.chunks_b64.iter().zip(&v.chunks_hex).enumerate()
        {
            let conf_chunk = decode_hex(&format!("{ctx}: chunksHex[{i}]"), conf_chunk_hex);
            let proto_chunk = decode_b64(&format!("{ctx}: chunks_b64[{i}]"), proto_chunk_b64);
            assert_bytes_eq(
                &format!("{ctx}: chunk {i} bytes"),
                &conf_chunk,
                &proto_chunk,
            );
        }

        let conf_is_error = v.expect_error.unwrap_or(false);
        let proto_is_error = proto_v.expect_error.unwrap_or(false);
        assert_eq!(proto_is_error, conf_is_error, "{ctx}: expectError drift");

        if conf_is_error {
            assert_eq!(
                proto_v.error_contains.as_deref(),
                v.error_contains.as_deref(),
                "{ctx}: errorContains drift"
            );
            continue;
        }

        let conf_frames = v
            .expect_frames
            .as_ref()
            .unwrap_or_else(|| panic!("{ctx}: conformance missing expectFrames"));
        let proto_frames = proto_v
            .expect_frames
            .as_ref()
            .unwrap_or_else(|| panic!("{ctx}: protocol-vectors missing expectFrames"));

        assert_eq!(
            proto_frames.len(),
            conf_frames.len(),
            "{ctx}: expectFrames length drift"
        );
        for (i, (proto_f, conf_f)) in proto_frames.iter().zip(conf_frames).enumerate() {
            let fctx = format!("{ctx}: expectFrames[{i}]");
            assert_eq!(proto_f.msg_type, conf_f.msg_type, "{fctx}: msgType drift");
            assert_eq!(
                proto_f.stream_id, conf_f.stream_id,
                "{fctx}: streamId drift"
            );

            let conf_payload = decode_hex(&format!("{fctx}: payloadHex"), &conf_f.payload_hex);
            let proto_payload = decode_b64(&format!("{fctx}: payload_b64"), &proto_f.payload_b64);
            assert_bytes_eq(
                &format!("{fctx}: payload bytes"),
                &conf_payload,
                &proto_payload,
            );
        }
    }
}

#[test]
fn udp_relay_vectors_do_not_drift() {
    let proto_path = protocol_udp_relay_path();
    let conf_path = conformance_vectors_v2_path();

    let proto: ProtocolUdpRelayVectorsFile = read_json(&proto_path);
    assert_eq!(
        proto.schema, 1,
        "unexpected protocol udp-relay vector schema"
    );

    let conf: ConformanceVectorsFileV2 = read_json(&conf_path);
    assert_eq!(
        conf.version, 2,
        "unexpected conformance vector schema version"
    );
    assert_eq!(
        conf.aero_udp_relay_v1v2.schema, 1,
        "unexpected conformance udp-relay schema"
    );

    let mut proto_vectors: BTreeMap<&str, &ProtocolUdpRelayVector> = BTreeMap::new();
    for v in &proto.vectors {
        let name = v.name.as_str();
        if proto_vectors.insert(name, v).is_some() {
            panic!("duplicate protocol udp-relay vector name: {name}");
        }
    }

    for v in &conf.aero_udp_relay_v1v2.vectors {
        let ctx = format!(
            "udp-relay vector {:?} (protocol-vectors vs conformance)",
            v.name
        );
        let proto_v = proto_vectors
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (vectors)"));

        let proto_is_error = proto_v.expect_error.unwrap_or(false);
        let conf_is_error = v.expect_error.unwrap_or(false);
        assert_eq!(proto_is_error, conf_is_error, "{ctx}: expectError drift");
        if conf_is_error {
            assert_eq!(
                proto_v.error_contains.as_deref(),
                v.error_contains.as_deref(),
                "{ctx}: errorContains drift"
            );
        }
        assert_eq!(proto_v.version, v.version, "{ctx}: version drift");
        assert_eq!(proto_v.guest_port, v.guest_port, "{ctx}: guestPort drift");
        assert_eq!(
            proto_v.remote_ip.as_deref(),
            v.remote_ip.as_deref(),
            "{ctx}: remoteIp drift"
        );
        assert_eq!(
            proto_v.remote_port, v.remote_port,
            "{ctx}: remotePort drift"
        );

        let conf_frame = decode_hex(&format!("{ctx}: frameHex"), &v.frame_hex);
        let proto_frame = decode_b64(&format!("{ctx}: frame_b64"), &proto_v.frame_b64);
        assert_bytes_eq(&format!("{ctx}: frame bytes"), &conf_frame, &proto_frame);

        match (&v.payload_hex, &proto_v.payload_b64) {
            (None, None) => {}
            (Some(_), None) => panic!("{ctx}: missing payload_b64 in protocol-vectors"),
            (None, Some(_)) => panic!("{ctx}: unexpected payload_b64 in protocol-vectors"),
            (Some(conf_payload_hex), Some(proto_payload_b64)) => {
                let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), conf_payload_hex);
                let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), proto_payload_b64);
                assert_bytes_eq(
                    &format!("{ctx}: payload bytes"),
                    &conf_payload,
                    &proto_payload,
                );
            }
        }
    }
}
