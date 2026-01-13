use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ConformanceVectorsV2File {
    version: u32,
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
    open_payloads: Vec<ConformanceTcpMuxPayloadVector>,
    #[serde(rename = "closePayloads")]
    close_payloads: Vec<ConformanceTcpMuxPayloadVector>,
    #[serde(rename = "errorPayloads")]
    error_payloads: Vec<ConformanceTcpMuxErrorPayloadVector>,
    #[serde(rename = "parserStreams")]
    parser_streams: Vec<ConformanceTcpMuxParserStreamVector>,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxFrameVector {
    name: String,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
    #[serde(rename = "frameHex")]
    frame_hex: String,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxPayloadVector {
    name: String,
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
struct ConformanceTcpMuxErrorPayloadVector {
    name: String,
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
    #[serde(rename = "payloadHex")]
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxVectorsFile {
    schema: u32,
    frames: Vec<ProtocolTcpMuxFrameVector>,
    #[serde(rename = "openPayloads")]
    open_payloads: Vec<ProtocolTcpMuxPayloadVector>,
    #[serde(rename = "closePayloads")]
    close_payloads: Vec<ProtocolTcpMuxPayloadVector>,
    #[serde(rename = "errorPayloads")]
    error_payloads: Vec<ProtocolTcpMuxErrorPayloadVector>,
    #[serde(rename = "parserStreams")]
    parser_streams: Vec<ProtocolTcpMuxParserStreamVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxFrameVector {
    name: String,
    #[serde(rename = "payload_b64")]
    payload_b64: String,
    #[serde(rename = "frame_b64")]
    frame_b64: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxPayloadVector {
    name: String,
    #[serde(rename = "payload_b64")]
    payload_b64: String,
}

#[derive(Debug, Deserialize)]
struct ProtocolTcpMuxErrorPayloadVector {
    name: String,
    #[serde(rename = "payload_b64")]
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
    #[serde(rename = "payload_b64")]
    payload_b64: String,
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
    #[serde(rename = "payloadHex")]
    payload_hex: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProtocolUdpRelayVectorsFile {
    schema: u32,
    vectors: Vec<ProtocolUdpRelayVector>,
}

#[derive(Debug, Deserialize)]
struct ProtocolUdpRelayVector {
    name: String,
    #[serde(rename = "frame_b64")]
    frame_b64: String,
    #[serde(rename = "payload_b64")]
    payload_b64: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn protocol_tcp_mux_vectors_path() -> PathBuf {
    repo_root().join("protocol-vectors/tcp-mux-v1.json")
}

fn protocol_udp_relay_vectors_path() -> PathBuf {
    repo_root().join("protocol-vectors/udp-relay.json")
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
                panic!("{ctx}: invalid base64 character 0x{c:02x} ('{}')", c as char)
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

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    let raw = fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path:?}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("parse {path:?}: {err}"))
}

/// Prevent accidental protocol drift between the older `protocol-vectors/` set (base64) and the
/// canonical `crates/conformance/test-vectors/aero-vectors-v2.json` set (hex) for tcp-mux.
#[test]
fn tcp_mux_vectors_do_not_drift_v2() {
    let proto_path = protocol_tcp_mux_vectors_path();
    let conf_path = conformance_vectors_v2_path();

    let proto: ProtocolTcpMuxVectorsFile = read_json(&proto_path);
    assert_eq!(proto.schema, 1, "unexpected protocol tcp-mux vector schema");

    let conf: ConformanceVectorsV2File = read_json(&conf_path);
    assert_eq!(conf.version, 2, "unexpected conformance vector schema version");
    assert_eq!(
        conf.aero_tcp_mux_v1.schema, 1,
        "unexpected conformance tcp-mux vector schema"
    );

    let mut proto_frames_by_name: BTreeMap<&str, &ProtocolTcpMuxFrameVector> = BTreeMap::new();
    for v in &proto.frames {
        let name = v.name.as_str();
        if proto_frames_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux frame vector name: {name}");
        }
    }

    let mut proto_open_by_name: BTreeMap<&str, &ProtocolTcpMuxPayloadVector> = BTreeMap::new();
    for v in &proto.open_payloads {
        let name = v.name.as_str();
        if proto_open_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux openPayload vector name: {name}");
        }
    }

    let mut proto_close_by_name: BTreeMap<&str, &ProtocolTcpMuxPayloadVector> = BTreeMap::new();
    for v in &proto.close_payloads {
        let name = v.name.as_str();
        if proto_close_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux closePayload vector name: {name}");
        }
    }

    let mut proto_error_payload_by_name: BTreeMap<&str, &ProtocolTcpMuxErrorPayloadVector> =
        BTreeMap::new();
    for v in &proto.error_payloads {
        let name = v.name.as_str();
        if proto_error_payload_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux errorPayload vector name: {name}");
        }
    }

    let mut proto_parser_stream_by_name: BTreeMap<&str, &ProtocolTcpMuxParserStreamVector> =
        BTreeMap::new();
    for v in &proto.parser_streams {
        let name = v.name.as_str();
        if proto_parser_stream_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol tcp-mux parserStream vector name: {name}");
        }
    }

    // Compare all canonical conformance vectors to their protocol-vectors counterparts.
    // We intentionally only require the canonical ones to exist in the protocol-vectors file; the
    // protocol-vectors file may carry extra vectors for older consumers / additional coverage.

    for v in &conf.aero_tcp_mux_v1.frames {
        let ctx = format!("tcp-mux frame vector {:?} (protocol-vectors vs conformance v2)", v.name);
        let proto_v = proto_frames_by_name.get(v.name.as_str()).copied().unwrap_or_else(|| {
            panic!("{ctx}: missing entry in {proto_path:?} (frames)")
        });

        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(&format!("{ctx}: payload bytes drift"), &conf_payload, &proto_payload);

        let conf_frame = decode_hex(&format!("{ctx}: frameHex"), &v.frame_hex);
        let proto_frame = decode_b64(&format!("{ctx}: frame_b64"), &proto_v.frame_b64);
        assert_bytes_eq(&format!("{ctx}: frame bytes drift"), &conf_frame, &proto_frame);
    }

    for v in &conf.aero_tcp_mux_v1.open_payloads {
        let ctx = format!(
            "tcp-mux openPayload vector {:?} (protocol-vectors vs conformance v2)",
            v.name
        );
        let proto_v = proto_open_by_name.get(v.name.as_str()).copied().unwrap_or_else(|| {
            panic!("{ctx}: missing entry in {proto_path:?} (openPayloads)")
        });

        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(&format!("{ctx}: payload bytes drift"), &conf_payload, &proto_payload);
    }

    for v in &conf.aero_tcp_mux_v1.close_payloads {
        let ctx = format!(
            "tcp-mux closePayload vector {:?} (protocol-vectors vs conformance v2)",
            v.name
        );
        let proto_v = proto_close_by_name.get(v.name.as_str()).copied().unwrap_or_else(|| {
            panic!("{ctx}: missing entry in {proto_path:?} (closePayloads)")
        });

        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(&format!("{ctx}: payload bytes drift"), &conf_payload, &proto_payload);
    }

    for v in &conf.aero_tcp_mux_v1.error_payloads {
        let ctx = format!(
            "tcp-mux errorPayload vector {:?} (protocol-vectors vs conformance v2)",
            v.name
        );
        let proto_v = proto_error_payload_by_name
            .get(v.name.as_str())
            .copied()
            .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (errorPayloads)"));

        let conf_payload = decode_hex(&format!("{ctx}: payloadHex"), &v.payload_hex);
        let proto_payload = decode_b64(&format!("{ctx}: payload_b64"), &proto_v.payload_b64);
        assert_bytes_eq(&format!("{ctx}: payload bytes drift"), &conf_payload, &proto_payload);

        let conf_expect_error = v.expect_error.unwrap_or(false);
        let proto_expect_error = proto_v.expect_error.unwrap_or(false);
        assert_eq!(
            proto_expect_error, conf_expect_error,
            "{ctx}: expectError drift (conformance={conf_expect_error}, protocol={proto_expect_error})"
        );

        assert_eq!(
            proto_v.error_contains.as_deref(),
            v.error_contains.as_deref(),
            "{ctx}: errorContains drift (conformance={:?}, protocol={:?})",
            v.error_contains,
            proto_v.error_contains
        );
    }

    for v in &conf.aero_tcp_mux_v1.parser_streams {
        let ctx = format!(
            "tcp-mux parserStream vector {:?} (protocol-vectors vs conformance v2)",
            v.name
        );
        let proto_v =
            proto_parser_stream_by_name
                .get(v.name.as_str())
                .copied()
                .unwrap_or_else(|| panic!("{ctx}: missing entry in {proto_path:?} (parserStreams)"));

        assert_eq!(
            proto_v.chunks_b64.len(),
            v.chunks_hex.len(),
            "{ctx}: chunks length drift (conformance={}, protocol={})",
            v.chunks_hex.len(),
            proto_v.chunks_b64.len()
        );
        for (i, (conf_chunk_hex, proto_chunk_b64)) in
            v.chunks_hex.iter().zip(&proto_v.chunks_b64).enumerate()
        {
            let conf_chunk = decode_hex(&format!("{ctx}: chunksHex[{i}]"), conf_chunk_hex);
            let proto_chunk = decode_b64(&format!("{ctx}: chunks_b64[{i}]"), proto_chunk_b64);
            assert_bytes_eq(
                &format!("{ctx}: chunk[{i}] bytes drift"),
                &conf_chunk,
                &proto_chunk,
            );
        }

        match (&v.expect_frames, &proto_v.expect_frames) {
            (Some(conf_frames), Some(proto_frames)) => {
                assert_eq!(
                    proto_frames.len(),
                    conf_frames.len(),
                    "{ctx}: expectFrames length drift (conformance={}, protocol={})",
                    conf_frames.len(),
                    proto_frames.len()
                );
                for (i, (conf_f, proto_f)) in
                    conf_frames.iter().zip(proto_frames).enumerate()
                {
                    let conf_payload = decode_hex(
                        &format!("{ctx}: expectFrames[{i}].payloadHex"),
                        &conf_f.payload_hex,
                    );
                    let proto_payload = decode_b64(
                        &format!("{ctx}: expectFrames[{i}].payload_b64"),
                        &proto_f.payload_b64,
                    );
                    assert_bytes_eq(
                        &format!("{ctx}: expectFrames[{i}] payload bytes drift"),
                        &conf_payload,
                        &proto_payload,
                    );
                }
            }
            (None, None) => {}
            (Some(_), None) => panic!("{ctx}: expectFrames drift (present in conformance, missing in protocol-vectors)"),
            (None, Some(_)) => panic!("{ctx}: expectFrames drift (missing in conformance, present in protocol-vectors)"),
        }

        let conf_expect_error = v.expect_error.unwrap_or(false);
        let proto_expect_error = proto_v.expect_error.unwrap_or(false);
        assert_eq!(
            proto_expect_error, conf_expect_error,
            "{ctx}: expectError drift (conformance={conf_expect_error}, protocol={proto_expect_error})"
        );

        assert_eq!(
            proto_v.error_contains.as_deref(),
            v.error_contains.as_deref(),
            "{ctx}: errorContains drift (conformance={:?}, protocol={:?})",
            v.error_contains,
            proto_v.error_contains
        );
    }
}

/// Prevent accidental protocol drift between the older `protocol-vectors/` set (base64) and the
/// canonical `crates/conformance/test-vectors/aero-vectors-v2.json` set (hex) for udp-relay.
#[test]
fn udp_relay_vectors_do_not_drift_v2() {
    let proto_path = protocol_udp_relay_vectors_path();
    let conf_path = conformance_vectors_v2_path();

    let proto: ProtocolUdpRelayVectorsFile = read_json(&proto_path);
    assert_eq!(proto.schema, 1, "unexpected protocol udp-relay vector schema");

    let conf: ConformanceVectorsV2File = read_json(&conf_path);
    assert_eq!(conf.version, 2, "unexpected conformance vector schema version");
    assert_eq!(
        conf.aero_udp_relay_v1v2.schema, 1,
        "unexpected conformance udp-relay vector schema"
    );

    let mut proto_by_name: BTreeMap<&str, &ProtocolUdpRelayVector> = BTreeMap::new();
    for v in &proto.vectors {
        let name = v.name.as_str();
        if proto_by_name.insert(name, v).is_some() {
            panic!("duplicate protocol udp-relay vector name: {name}");
        }
    }

    for v in &conf.aero_udp_relay_v1v2.vectors {
        let ctx = format!(
            "udp-relay vector {:?} (protocol-vectors vs conformance v2)",
            v.name
        );
        let proto_v = proto_by_name.get(v.name.as_str()).copied().unwrap_or_else(|| {
            panic!("{ctx}: missing entry in {proto_path:?} (vectors)")
        });

        let conf_frame = decode_hex(&format!("{ctx}: frameHex"), &v.frame_hex);
        let proto_frame = decode_b64(&format!("{ctx}: frame_b64"), &proto_v.frame_b64);
        assert_bytes_eq(&format!("{ctx}: frame bytes drift"), &conf_frame, &proto_frame);

        match (&v.payload_hex, &proto_v.payload_b64) {
            (Some(conf_payload_hex), Some(proto_payload_b64)) => {
                let conf_payload =
                    decode_hex(&format!("{ctx}: payloadHex"), conf_payload_hex.as_str());
                let proto_payload =
                    decode_b64(&format!("{ctx}: payload_b64"), proto_payload_b64.as_str());
                assert_bytes_eq(
                    &format!("{ctx}: payload bytes drift"),
                    &conf_payload,
                    &proto_payload,
                );
            }
            (None, None) => {}
            (Some(_), None) => panic!(
                "{ctx}: payload drift (present in conformance, missing in protocol-vectors)"
            ),
            (None, Some(_)) => panic!(
                "{ctx}: payload drift (missing in conformance, present in protocol-vectors)"
            ),
        }

        let conf_expect_error = v.expect_error.unwrap_or(false);
        let proto_expect_error = proto_v.expect_error.unwrap_or(false);
        assert_eq!(
            proto_expect_error, conf_expect_error,
            "{ctx}: expectError drift (conformance={conf_expect_error}, protocol={proto_expect_error})"
        );

        assert_eq!(
            proto_v.error_contains.as_deref(),
            v.error_contains.as_deref(),
            "{ctx}: errorContains drift (conformance={:?}, protocol={:?})",
            v.error_contains,
            proto_v.error_contains
        );
    }
}

