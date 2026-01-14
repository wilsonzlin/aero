use std::net::IpAddr;
use std::path::PathBuf;

use aero_udp_relay_protocol::{
    decode_datagram, encode_datagram, AddressFamily, Datagram, FramingVersion, TransportFlags,
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
    version: Option<u8>,
    #[serde(rename = "frame_b64")]
    frame_b64: String,
    #[serde(rename = "guestPort")]
    guest_port: Option<u16>,
    #[serde(rename = "remoteIp")]
    remote_ip: Option<String>,
    #[serde(rename = "remotePort")]
    remote_port: Option<u16>,
    #[serde(rename = "payload_b64")]
    payload_b64: Option<String>,
    #[serde(rename = "expectError")]
    expect_error: Option<bool>,
    #[serde(rename = "errorContains")]
    error_contains: Option<String>,
}

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol-vectors/udp-relay.json")
}

fn decode_b64(b64: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("base64 decode")
}

#[test]
fn udp_relay_vectors() {
    let raw = std::fs::read_to_string(vectors_path()).expect("read vectors file");
    let file: VectorsFile = serde_json::from_str(&raw).expect("parse vectors JSON");
    assert_eq!(file.schema, 1);

    for v in file.vectors {
        let frame = decode_b64(&v.frame_b64);
        if v.expect_error.unwrap_or(false) {
            let err =
                decode_datagram(&frame).expect_err(&format!("vector {} expected error", v.name));
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

        let version = v.version.expect("version");
        let guest_port = v.guest_port.expect("guestPort");
        let remote_ip: IpAddr = v
            .remote_ip
            .as_deref()
            .expect("remoteIp")
            .parse()
            .expect("ip");
        let remote_port = v.remote_port.expect("remotePort");
        let payload = decode_b64(v.payload_b64.as_deref().expect("payload_b64"));

        let decoded =
            decode_datagram(&frame).unwrap_or_else(|err| panic!("vector {} decode: {err}", v.name));

        let (want_version, want_af) = match version {
            1 => (FramingVersion::V1, AddressFamily::Ipv4),
            2 => (
                FramingVersion::V2,
                match remote_ip {
                    IpAddr::V4(_) => AddressFamily::Ipv4,
                    IpAddr::V6(_) => AddressFamily::Ipv6,
                },
            ),
            other => panic!("vector {}: unsupported version {other}", v.name),
        };

        assert_eq!(decoded.transport.version, want_version, "vector {}", v.name);
        assert_eq!(
            decoded.transport.address_family, want_af,
            "vector {}",
            v.name
        );
        assert_eq!(decoded.guest_port, guest_port, "vector {}", v.name);
        assert_eq!(decoded.remote_ip, remote_ip, "vector {}", v.name);
        assert_eq!(decoded.remote_port, remote_port, "vector {}", v.name);
        assert_eq!(decoded.payload, payload.as_slice(), "vector {}", v.name);

        let datagram = Datagram {
            guest_port,
            remote_ip,
            remote_port,
            payload: payload.as_slice(),
            transport: TransportFlags {
                version: want_version,
                address_family: want_af,
            },
        };
        let encoded = encode_datagram(datagram)
            .unwrap_or_else(|err| panic!("vector {} encode: {err}", v.name));
        assert_eq!(encoded, frame, "vector {}", v.name);
    }
}

#[test]
fn rejects_oversized_payloads() {
    let limits = aero_udp_relay_protocol::Limits { max_payload: 3 };

    let payload = [0u8, 1, 2, 3];
    let datagram = Datagram {
        guest_port: 1,
        remote_ip: IpAddr::from([127, 0, 0, 1]),
        remote_port: 2,
        payload: &payload,
        transport: TransportFlags::v1_ipv4(),
    };
    assert!(matches!(
        aero_udp_relay_protocol::encode_datagram_with_limits(datagram, &limits),
        Err(aero_udp_relay_protocol::EncodeError::PayloadTooLarge { .. })
    ));

    let mut frame = vec![
        0x00, 0x01, // guest_port
        127, 0, 0, 1, // remote_ipv4
        0x00, 0x02, // remote_port
    ];
    frame.extend_from_slice(&payload);
    assert!(matches!(
        aero_udp_relay_protocol::decode_datagram_with_limits(&frame, &limits),
        Err(aero_udp_relay_protocol::DecodeError::PayloadTooLarge { .. })
    ));
}
