#![forbid(unsafe_code)]

//! Aero UDP relay framing codec (v1 and v2).
//!
//! This crate implements the binary datagram framing used by the WebRTC UDP relay
//! (`proxy/webrtc-udp-relay`) and its WebSocket `/udp` fallback.
//!
//! The normative spec lives at `proxy/webrtc-udp-relay/PROTOCOL.md`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// v1 header length in bytes.
pub const UDP_RELAY_V1_HEADER_LEN: usize = 8;

/// v2 frame magic prefix byte.
pub const UDP_RELAY_V2_MAGIC: u8 = 0xA2;
/// v2 frame version byte.
pub const UDP_RELAY_V2_VERSION: u8 = 0x02;

/// v2 address family byte for IPv4.
pub const UDP_RELAY_V2_AF_IPV4: u8 = 0x04;
/// v2 address family byte for IPv6.
pub const UDP_RELAY_V2_AF_IPV6: u8 = 0x06;

/// v2 message type byte for datagram frames.
pub const UDP_RELAY_V2_TYPE_DATAGRAM: u8 = 0x00;

/// Default maximum datagram payload size in bytes.
///
/// Keep in sync with:
/// - `proxy/webrtc-udp-relay/internal/udpproto.DefaultMaxPayload`
/// - `proxy/webrtc-udp-relay/PROTOCOL.md`
pub const UDP_RELAY_DEFAULT_MAX_PAYLOAD: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_payload: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_payload: UDP_RELAY_DEFAULT_MAX_PAYLOAD,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramingVersion {
    V1 = 1,
    V2 = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    Ipv4 = 4,
    Ipv6 = 6,
}

impl AddressFamily {
    fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => AddressFamily::Ipv4,
            IpAddr::V6(_) => AddressFamily::Ipv6,
        }
    }
}

/// Transport metadata associated with a decoded datagram frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportFlags {
    pub version: FramingVersion,
    pub address_family: AddressFamily,
}

impl TransportFlags {
    pub const fn v1_ipv4() -> Self {
        Self {
            version: FramingVersion::V1,
            address_family: AddressFamily::Ipv4,
        }
    }

    pub const fn v2_ipv4() -> Self {
        Self {
            version: FramingVersion::V2,
            address_family: AddressFamily::Ipv4,
        }
    }

    pub const fn v2_ipv6() -> Self {
        Self {
            version: FramingVersion::V2,
            address_family: AddressFamily::Ipv6,
        }
    }
}

/// A decoded UDP relay datagram frame.
///
/// Field semantics match `proxy/webrtc-udp-relay/PROTOCOL.md`:
/// - `guest_port` is always the guest-side UDP port (source on outbound frames,
///   destination on inbound frames).
/// - `remote_ip`/`remote_port` identify the remote endpoint (destination on
///   outbound frames, source on inbound frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Datagram<'a> {
    pub guest_port: u16,
    pub remote_ip: IpAddr,
    pub remote_port: u16,
    pub payload: &'a [u8],
    pub transport: TransportFlags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooShort { len: usize, min: usize },
    PayloadTooLarge { len: usize, max: usize },
    V2UnsupportedMessageType { msg_type: u8 },
    V2UnknownAddressFamily { address_family: u8 },
    V2MissingPrefix,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::TooShort { len, min } => write!(f, "frame too short: {len} < {min}"),
            DecodeError::PayloadTooLarge { len, max } => {
                write!(f, "payload too large: {len} > {max}")
            }
            DecodeError::V2UnsupportedMessageType { msg_type } => write!(
                f,
                "v2 unsupported message type: 0x{msg_type:02x} (expected 0x{UDP_RELAY_V2_TYPE_DATAGRAM:02x})"
            ),
            DecodeError::V2UnknownAddressFamily { address_family } => {
                write!(f, "v2 unknown address family: 0x{address_family:02x}")
            }
            DecodeError::V2MissingPrefix => write!(f, "missing v2 prefix"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    PayloadTooLarge {
        len: usize,
        max: usize,
    },
    V1RequiresIpv4,
    TransportAddressFamilyMismatch {
        transport: AddressFamily,
        ip: AddressFamily,
    },
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EncodeError::PayloadTooLarge { len, max } => {
                write!(f, "payload too large: {len} > {max}")
            }
            EncodeError::V1RequiresIpv4 => write!(f, "v1 only supports IPv4"),
            EncodeError::TransportAddressFamilyMismatch { transport, ip } => write!(
                f,
                "transport address family mismatch: transport={transport:?} ip={ip:?}"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

fn is_v2_prefix(buf: &[u8]) -> bool {
    buf.len() >= 2 && buf[0] == UDP_RELAY_V2_MAGIC && buf[1] == UDP_RELAY_V2_VERSION
}

/// Decode a UDP relay datagram frame using the v2 prefix heuristic:
///
/// - If the frame starts with `0xA2 0x02`, decode as v2.
/// - Otherwise, decode as v1.
pub fn decode_datagram(frame: &[u8]) -> Result<Datagram<'_>, DecodeError> {
    decode_datagram_with_limits(frame, &Limits::default())
}

pub fn decode_datagram_with_limits<'a>(
    frame: &'a [u8],
    limits: &Limits,
) -> Result<Datagram<'a>, DecodeError> {
    if is_v2_prefix(frame) {
        return decode_v2_datagram_with_limits(frame, limits);
    }
    decode_v1_datagram_with_limits(frame, limits)
}

/// Decode a v1 UDP relay datagram frame (IPv4-only).
pub fn decode_v1_datagram_with_limits<'a>(
    frame: &'a [u8],
    limits: &Limits,
) -> Result<Datagram<'a>, DecodeError> {
    if frame.len() < UDP_RELAY_V1_HEADER_LEN {
        return Err(DecodeError::TooShort {
            len: frame.len(),
            min: UDP_RELAY_V1_HEADER_LEN,
        });
    }

    let payload = &frame[UDP_RELAY_V1_HEADER_LEN..];
    if payload.len() > limits.max_payload {
        return Err(DecodeError::PayloadTooLarge {
            len: payload.len(),
            max: limits.max_payload,
        });
    }

    let guest_port = u16::from_be_bytes([frame[0], frame[1]]);
    let remote_ip = Ipv4Addr::new(frame[2], frame[3], frame[4], frame[5]);
    let remote_port = u16::from_be_bytes([frame[6], frame[7]]);

    Ok(Datagram {
        guest_port,
        remote_ip: IpAddr::V4(remote_ip),
        remote_port,
        payload,
        transport: TransportFlags::v1_ipv4(),
    })
}

/// Decode a v2 UDP relay datagram frame (IPv4 or IPv6).
pub fn decode_v2_datagram_with_limits<'a>(
    frame: &'a [u8],
    limits: &Limits,
) -> Result<Datagram<'a>, DecodeError> {
    if frame.len() < 12 {
        return Err(DecodeError::TooShort {
            len: frame.len(),
            min: 12,
        });
    }
    if !is_v2_prefix(frame) {
        return Err(DecodeError::V2MissingPrefix);
    }

    let af = frame[2];
    let msg_type = frame[3];
    if msg_type != UDP_RELAY_V2_TYPE_DATAGRAM {
        return Err(DecodeError::V2UnsupportedMessageType { msg_type });
    }

    let (address_family, ip_len) = match af {
        UDP_RELAY_V2_AF_IPV4 => (AddressFamily::Ipv4, 4usize),
        UDP_RELAY_V2_AF_IPV6 => (AddressFamily::Ipv6, 16usize),
        _ => return Err(DecodeError::V2UnknownAddressFamily { address_family: af }),
    };

    let header_len = 4usize
        .checked_add(2)
        .and_then(|n| n.checked_add(ip_len))
        .and_then(|n| n.checked_add(2))
        .unwrap_or(usize::MAX);
    if frame.len() < header_len {
        return Err(DecodeError::TooShort {
            len: frame.len(),
            min: header_len,
        });
    }

    let guest_port = u16::from_be_bytes([frame[4], frame[5]]);

    let ip_off = 6usize;
    let remote_ip = match address_family {
        AddressFamily::Ipv4 => {
            let ip4 = Ipv4Addr::new(
                frame[ip_off],
                frame[ip_off + 1],
                frame[ip_off + 2],
                frame[ip_off + 3],
            );
            IpAddr::V4(ip4)
        }
        AddressFamily::Ipv6 => {
            let ip_bytes: [u8; 16] =
                frame[ip_off..ip_off + 16]
                    .try_into()
                    .map_err(|_e| DecodeError::TooShort {
                        len: frame.len(),
                        min: header_len,
                    })?;
            IpAddr::V6(Ipv6Addr::from(ip_bytes))
        }
    };

    let port_off = ip_off + ip_len;
    let remote_port = u16::from_be_bytes([frame[port_off], frame[port_off + 1]]);

    let payload = &frame[header_len..];
    if payload.len() > limits.max_payload {
        return Err(DecodeError::PayloadTooLarge {
            len: payload.len(),
            max: limits.max_payload,
        });
    }

    Ok(Datagram {
        guest_port,
        remote_ip,
        remote_port,
        payload,
        transport: TransportFlags {
            version: FramingVersion::V2,
            address_family,
        },
    })
}

/// Encode a datagram frame using `d.transport.version` to select v1 vs v2 framing.
pub fn encode_datagram(d: Datagram<'_>) -> Result<Vec<u8>, EncodeError> {
    encode_datagram_with_limits(d, &Limits::default())
}

pub fn encode_datagram_with_limits(
    d: Datagram<'_>,
    limits: &Limits,
) -> Result<Vec<u8>, EncodeError> {
    match d.transport.version {
        FramingVersion::V1 => encode_v1_datagram_with_limits(d, limits),
        FramingVersion::V2 => encode_v2_datagram_with_limits(d, limits),
    }
}

/// Encode a v1 UDP relay datagram frame (IPv4-only).
pub fn encode_v1_datagram_with_limits(
    d: Datagram<'_>,
    limits: &Limits,
) -> Result<Vec<u8>, EncodeError> {
    if d.payload.len() > limits.max_payload {
        return Err(EncodeError::PayloadTooLarge {
            len: d.payload.len(),
            max: limits.max_payload,
        });
    }
    let ip_family = AddressFamily::from_ip(d.remote_ip);
    if ip_family != AddressFamily::Ipv4 {
        return Err(EncodeError::V1RequiresIpv4);
    }
    if d.transport.address_family != AddressFamily::Ipv4 {
        return Err(EncodeError::TransportAddressFamilyMismatch {
            transport: d.transport.address_family,
            ip: ip_family,
        });
    }

    let ip4 = match d.remote_ip {
        IpAddr::V4(ip) => ip.octets(),
        IpAddr::V6(_) => return Err(EncodeError::V1RequiresIpv4),
    };

    let mut out = Vec::with_capacity(UDP_RELAY_V1_HEADER_LEN.saturating_add(d.payload.len()));
    out.extend_from_slice(&d.guest_port.to_be_bytes());
    out.extend_from_slice(&ip4);
    out.extend_from_slice(&d.remote_port.to_be_bytes());
    out.extend_from_slice(d.payload);
    Ok(out)
}

/// Encode a v2 UDP relay datagram frame (IPv4 or IPv6).
pub fn encode_v2_datagram_with_limits(
    d: Datagram<'_>,
    limits: &Limits,
) -> Result<Vec<u8>, EncodeError> {
    if d.payload.len() > limits.max_payload {
        return Err(EncodeError::PayloadTooLarge {
            len: d.payload.len(),
            max: limits.max_payload,
        });
    }

    let ip_family = AddressFamily::from_ip(d.remote_ip);
    if d.transport.address_family != ip_family {
        return Err(EncodeError::TransportAddressFamilyMismatch {
            transport: d.transport.address_family,
            ip: ip_family,
        });
    }

    let (af_byte, ip_bytes): (u8, Vec<u8>) = match d.remote_ip {
        IpAddr::V4(ip) => (UDP_RELAY_V2_AF_IPV4, ip.octets().to_vec()),
        IpAddr::V6(ip) => (UDP_RELAY_V2_AF_IPV6, ip.octets().to_vec()),
    };
    let ip_len = ip_bytes.len();

    let header_len = 4usize
        .checked_add(2)
        .and_then(|n| n.checked_add(ip_len))
        .and_then(|n| n.checked_add(2))
        .unwrap_or(usize::MAX);

    let mut out = Vec::with_capacity(header_len.saturating_add(d.payload.len()));
    out.push(UDP_RELAY_V2_MAGIC);
    out.push(UDP_RELAY_V2_VERSION);
    out.push(af_byte);
    out.push(UDP_RELAY_V2_TYPE_DATAGRAM);
    out.extend_from_slice(&d.guest_port.to_be_bytes());
    out.extend_from_slice(&ip_bytes);
    out.extend_from_slice(&d.remote_port.to_be_bytes());
    out.extend_from_slice(d.payload);
    Ok(out)
}
