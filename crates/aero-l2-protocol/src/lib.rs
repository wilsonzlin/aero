//! Aero L2 tunnel protocol codec.
//!
//! This is a small shared library intended to keep proxy-side implementations
//! consistent with the browser-side TypeScript codec in
//! `web/src/shared/l2TunnelProtocol.ts` and the normative spec in
//! `docs/l2-tunnel-protocol.md`.

pub const L2_TUNNEL_MAGIC: u8 = 0xA2;
pub const L2_TUNNEL_VERSION: u8 = 0x03;

pub const L2_TUNNEL_HEADER_LEN: usize = 4;

pub const L2_TUNNEL_TYPE_FRAME: u8 = 0x00;
pub const L2_TUNNEL_TYPE_PING: u8 = 0x01;
pub const L2_TUNNEL_TYPE_PONG: u8 = 0x02;
pub const L2_TUNNEL_TYPE_ERROR: u8 = 0x7F;

pub const L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD: usize = 2048;
pub const L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_frame_payload: usize,
    pub max_control_payload: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_payload: L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
            max_control_payload: L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct L2Message<'a> {
    pub version: u8,
    pub msg_type: u8,
    pub flags: u8,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooShort { len: usize },
    InvalidMagic { magic: u8 },
    UnsupportedVersion { version: u8 },
    PayloadTooLarge { len: usize, max: usize },
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::TooShort { len } => write!(f, "message too short: {len} < {L2_TUNNEL_HEADER_LEN}"),
            DecodeError::InvalidMagic { magic } => write!(f, "invalid magic: 0x{magic:02x}"),
            DecodeError::UnsupportedVersion { version } => write!(f, "unsupported version: 0x{version:02x}"),
            DecodeError::PayloadTooLarge { len, max } => write!(f, "payload too large: {len} > {max}"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    PayloadTooLarge { len: usize, max: usize },
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EncodeError::PayloadTooLarge { len, max } => write!(f, "payload too large: {len} > {max}"),
        }
    }
}

impl std::error::Error for EncodeError {}

fn max_payload_for_type(msg_type: u8, limits: &Limits) -> usize {
    if msg_type == L2_TUNNEL_TYPE_FRAME {
        limits.max_frame_payload
    } else {
        limits.max_control_payload
    }
}

pub fn encode_with_limits(msg_type: u8, flags: u8, payload: &[u8], limits: &Limits) -> Result<Vec<u8>, EncodeError> {
    let max = max_payload_for_type(msg_type, limits);
    if payload.len() > max {
        return Err(EncodeError::PayloadTooLarge {
            len: payload.len(),
            max,
        });
    }

    let mut out = vec![0u8; L2_TUNNEL_HEADER_LEN + payload.len()];
    out[0] = L2_TUNNEL_MAGIC;
    out[1] = L2_TUNNEL_VERSION;
    out[2] = msg_type;
    out[3] = flags;
    out[L2_TUNNEL_HEADER_LEN..].copy_from_slice(payload);
    Ok(out)
}

pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, EncodeError> {
    encode_with_limits(L2_TUNNEL_TYPE_FRAME, 0, payload, &Limits::default())
}

pub fn encode_ping(payload: Option<&[u8]>) -> Result<Vec<u8>, EncodeError> {
    encode_with_limits(
        L2_TUNNEL_TYPE_PING,
        0,
        payload.unwrap_or_default(),
        &Limits::default(),
    )
}

pub fn encode_pong(payload: Option<&[u8]>) -> Result<Vec<u8>, EncodeError> {
    encode_with_limits(
        L2_TUNNEL_TYPE_PONG,
        0,
        payload.unwrap_or_default(),
        &Limits::default(),
    )
}

pub fn decode_with_limits<'a>(buf: &'a [u8], limits: &Limits) -> Result<L2Message<'a>, DecodeError> {
    if buf.len() < L2_TUNNEL_HEADER_LEN {
        return Err(DecodeError::TooShort { len: buf.len() });
    }

    if buf[0] != L2_TUNNEL_MAGIC {
        return Err(DecodeError::InvalidMagic { magic: buf[0] });
    }
    if buf[1] != L2_TUNNEL_VERSION {
        return Err(DecodeError::UnsupportedVersion { version: buf[1] });
    }

    let msg_type = buf[2];
    let flags = buf[3];
    let payload = &buf[L2_TUNNEL_HEADER_LEN..];

    let max = max_payload_for_type(msg_type, limits);
    if payload.len() > max {
        return Err(DecodeError::PayloadTooLarge {
            len: payload.len(),
            max,
        });
    }

    Ok(L2Message {
        version: buf[1],
        msg_type,
        flags,
        payload,
    })
}

pub fn decode_message(buf: &[u8]) -> Result<L2Message<'_>, DecodeError> {
    decode_with_limits(buf, &Limits::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_frame() {
        let payload = [0u8, 1, 2, 3, 4, 5];
        let encoded = encode_frame(&payload).unwrap();
        let decoded = decode_message(&encoded).unwrap();
        assert_eq!(decoded.version, L2_TUNNEL_VERSION);
        assert_eq!(decoded.msg_type, L2_TUNNEL_TYPE_FRAME);
        assert_eq!(decoded.flags, 0);
        assert_eq!(decoded.payload, payload.as_slice());
    }

    #[test]
    fn roundtrip_ping_pong() {
        let payload = [9u8, 8, 7, 6];

        let ping_wire = encode_ping(Some(&payload)).unwrap();
        let ping = decode_message(&ping_wire).unwrap();
        assert_eq!(ping.msg_type, L2_TUNNEL_TYPE_PING);
        assert_eq!(ping.payload, payload.as_slice());

        let pong_wire = encode_pong(Some(&payload)).unwrap();
        let pong = decode_message(&pong_wire).unwrap();
        assert_eq!(pong.msg_type, L2_TUNNEL_TYPE_PONG);
        assert_eq!(pong.payload, payload.as_slice());
    }

    #[test]
    fn rejects_wrong_magic_and_version() {
        let mut encoded = encode_frame(&[1u8, 2, 3]).unwrap();
        encoded[0] = 0x00;
        assert!(matches!(decode_message(&encoded), Err(DecodeError::InvalidMagic { .. })));

        let mut encoded = encode_frame(&[1u8, 2, 3]).unwrap();
        encoded[1] = 0xFF;
        assert!(matches!(
            decode_message(&encoded),
            Err(DecodeError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn rejects_oversized_payloads() {
        let payload = vec![0u8; L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD + 1];
        assert!(matches!(
            encode_frame(&payload),
            Err(EncodeError::PayloadTooLarge { .. })
        ));

        let mut wire = Vec::with_capacity(L2_TUNNEL_HEADER_LEN + payload.len());
        wire.push(L2_TUNNEL_MAGIC);
        wire.push(L2_TUNNEL_VERSION);
        wire.push(L2_TUNNEL_TYPE_FRAME);
        wire.push(0);
        wire.extend_from_slice(&payload);
        assert!(matches!(
            decode_message(&wire),
            Err(DecodeError::PayloadTooLarge { .. })
        ));
    }
}
