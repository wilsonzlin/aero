#![forbid(unsafe_code)]

//! `aero-tcp-mux-v1` protocol codec.
//!
//! This crate provides a canonical Rust implementation of the TCP mux framing used by Aero.
//! The normative reference implementations live in:
//! - `backend/aero-gateway/src/protocol/tcpMux.ts`
//! - `tools/net-proxy-server/src/protocol.js`
//! - `web/src/net/tcpMuxProxy.ts` (client-side usage)
//!
//! Wire format (all integer fields big-endian):
//!
//! ```text
//! 0               1               5               9
//! +---------------+---------------+---------------+
//! | msg_type (u8) | stream_id(u32)|  len (u32)    |  header (9 bytes)
//! +---------------+---------------+---------------+
//! | payload (len bytes)                           |
//! +----------------------------------------------+
//! ```

use core::fmt;

pub const TCP_MUX_SUBPROTOCOL: &str = "aero-tcp-mux-v1";

pub const TCP_MUX_HEADER_LEN: usize = 9;

// Keep in sync with `backend/aero-gateway/src/protocol/tcpMux.ts`.
pub const TCP_MUX_MSG_TYPE_OPEN: u8 = 1;
pub const TCP_MUX_MSG_TYPE_DATA: u8 = 2;
pub const TCP_MUX_MSG_TYPE_CLOSE: u8 = 3;
pub const TCP_MUX_MSG_TYPE_ERROR: u8 = 4;
pub const TCP_MUX_MSG_TYPE_PING: u8 = 5;
pub const TCP_MUX_MSG_TYPE_PONG: u8 = 6;

// These match `backend/aero-gateway/src/config.ts` defaults.
pub const TCP_MUX_DEFAULT_MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_payload_len: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_payload_len: TCP_MUX_DEFAULT_MAX_PAYLOAD_LEN,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub msg_type: u8,
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPayload {
    pub host: String,
    pub port: u16,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorPayload {
    pub code: u16,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    FrameTooLarge {
        len: usize,
        max: usize,
    },
    FrameTooShort {
        len: usize,
    },
    FrameTruncatedPayload {
        expected: usize,
        got: usize,
    },
    FrameTrailingBytes {
        trailing: usize,
    },

    TruncatedStreamHeader {
        pending: usize,
    },
    TruncatedStreamPayload {
        pending: usize,
        payload_len: usize,
    },

    OpenHostTooLong {
        len: usize,
        max: usize,
    },
    OpenMetadataTooLong {
        len: usize,
        max: usize,
    },
    OpenInvalidPort {
        port: u16,
    },
    OpenPayloadTooShort {
        len: usize,
    },
    OpenPayloadTruncatedHost {
        host_len: usize,
        remaining: usize,
    },
    OpenPayloadTruncatedMetadata {
        metadata_len: usize,
        remaining: usize,
    },
    OpenPayloadTrailingBytes {
        trailing: usize,
    },

    ClosePayloadWrongLen {
        len: usize,
    },

    ErrorMessageTooLong {
        len: usize,
        max: usize,
    },
    ErrorPayloadTooShort {
        len: usize,
    },
    ErrorPayloadLengthMismatch {
        expected: usize,
        got: usize,
    },

    InvalidUtf8 {
        context: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::FrameTooLarge { len, max } => write!(f, "frame too large: {len} > {max}"),
            Error::FrameTooShort { len } => write!(
                f,
                "tcp-mux frame too short: {len} < {TCP_MUX_HEADER_LEN} (truncated header)"
            ),
            Error::FrameTruncatedPayload { expected, got } => write!(
                f,
                "tcp-mux frame truncated payload: expected {expected} bytes, got {got}"
            ),
            Error::FrameTrailingBytes { trailing } => {
                write!(f, "tcp-mux frame has trailing bytes: {trailing}")
            }

            Error::TruncatedStreamHeader { pending } => write!(
                f,
                "truncated tcp-mux frame stream (truncated header: {pending} pending bytes)"
            ),
            Error::TruncatedStreamPayload {
                pending,
                payload_len,
            } => write!(
                f,
                "truncated tcp-mux frame stream (truncated payload: {pending}/{payload_len} payload bytes)"
            ),

            Error::OpenHostTooLong { len, max } => write!(f, "host too long: {len} > {max}"),
            Error::OpenMetadataTooLong { len, max } => {
                write!(f, "metadata too long: {len} > {max}")
            }
            Error::OpenInvalidPort { port } => write!(f, "invalid port: {port}"),
            Error::OpenPayloadTooShort { len } => write!(f, "OPEN payload too short: {len}"),
            Error::OpenPayloadTruncatedHost {
                host_len,
                remaining,
            } => write!(
                f,
                "OPEN payload truncated (host): need {host_len} bytes, have {remaining}"
            ),
            Error::OpenPayloadTruncatedMetadata {
                metadata_len,
                remaining,
            } => write!(
                f,
                "OPEN payload truncated (metadata): need {metadata_len} bytes, have {remaining}"
            ),
            Error::OpenPayloadTrailingBytes { trailing } => {
                write!(f, "OPEN payload has trailing bytes: {trailing}")
            }

            Error::ClosePayloadWrongLen { len } => {
                write!(f, "CLOSE payload must be exactly 1 byte (got {len})")
            }

            Error::ErrorMessageTooLong { len, max } => {
                write!(f, "error message too long: {len} > {max}")
            }
            Error::ErrorPayloadTooShort { len } => write!(f, "ERROR payload too short: {len}"),
            Error::ErrorPayloadLengthMismatch { expected, got } => write!(
                f,
                "ERROR payload length mismatch: expected {expected}, got {got}"
            ),

            Error::InvalidUtf8 { context } => write!(f, "invalid UTF-8 in {context}"),
        }
    }
}

impl std::error::Error for Error {}

pub fn encode_frame_with_limits(
    msg_type: u8,
    stream_id: u32,
    payload: &[u8],
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
    if payload.len() > limits.max_payload_len {
        return Err(Error::FrameTooLarge {
            len: payload.len(),
            max: limits.max_payload_len,
        });
    }

    // Length is encoded as u32.
    if payload.len() > u32::MAX as usize {
        return Err(Error::FrameTooLarge {
            len: payload.len(),
            max: u32::MAX as usize,
        });
    }

    let mut out = Vec::with_capacity(TCP_MUX_HEADER_LEN + payload.len());
    out.push(msg_type);
    out.extend_from_slice(&stream_id.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn encode_frame(msg_type: u8, stream_id: u32, payload: &[u8]) -> Result<Vec<u8>, Error> {
    encode_frame_with_limits(msg_type, stream_id, payload, &Limits::default())
}

pub fn decode_frame_with_limits(buf: &[u8], limits: &Limits) -> Result<Frame, Error> {
    if buf.len() < TCP_MUX_HEADER_LEN {
        return Err(Error::FrameTooShort { len: buf.len() });
    }

    let msg_type = buf[0];
    let stream_id = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    let payload_len_u32 = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);
    let payload_len = payload_len_u32 as usize;

    if payload_len > limits.max_payload_len {
        return Err(Error::FrameTooLarge {
            len: payload_len,
            max: limits.max_payload_len,
        });
    }

    let expected_total =
        TCP_MUX_HEADER_LEN
            .checked_add(payload_len)
            .ok_or(Error::FrameTooLarge {
                len: payload_len,
                max: limits.max_payload_len,
            })?;

    if buf.len() < expected_total {
        return Err(Error::FrameTruncatedPayload {
            expected: expected_total,
            got: buf.len(),
        });
    }

    if buf.len() > expected_total {
        return Err(Error::FrameTrailingBytes {
            trailing: buf.len() - expected_total,
        });
    }

    Ok(Frame {
        msg_type,
        stream_id,
        payload: buf[TCP_MUX_HEADER_LEN..].to_vec(),
    })
}

pub fn decode_frame(buf: &[u8]) -> Result<Frame, Error> {
    decode_frame_with_limits(buf, &Limits::default())
}

/// Streaming parser for `aero-tcp-mux-v1` frames.
///
/// The parser is incremental and can accept arbitrary chunk boundaries.
/// It only allocates up to the configured `max_payload_len` per frame.
#[derive(Debug, Clone)]
pub struct FrameParser {
    limits: Limits,
    state: ParserState,
}

#[derive(Debug, Clone)]
enum ParserState {
    Header {
        buf: [u8; TCP_MUX_HEADER_LEN],
        filled: usize,
    },
    Payload {
        msg_type: u8,
        stream_id: u32,
        payload_len: usize,
        buf: Vec<u8>,
    },
}

impl FrameParser {
    pub fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    pub fn with_limits(limits: Limits) -> Self {
        Self {
            limits,
            state: ParserState::Header {
                buf: [0u8; TCP_MUX_HEADER_LEN],
                filled: 0,
            },
        }
    }

    pub fn push(&mut self, mut chunk: &[u8]) -> Result<Vec<Frame>, Error> {
        let mut frames = Vec::new();

        while !chunk.is_empty() {
            match &mut self.state {
                ParserState::Header { buf, filled } => {
                    let need = TCP_MUX_HEADER_LEN - *filled;
                    let take = need.min(chunk.len());
                    buf[*filled..*filled + take].copy_from_slice(&chunk[..take]);
                    *filled += take;
                    chunk = &chunk[take..];

                    if *filled < TCP_MUX_HEADER_LEN {
                        continue;
                    }

                    let msg_type = buf[0];
                    let stream_id = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
                    let payload_len_u32 = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);
                    let payload_len = payload_len_u32 as usize;

                    if payload_len > self.limits.max_payload_len {
                        return Err(Error::FrameTooLarge {
                            len: payload_len,
                            max: self.limits.max_payload_len,
                        });
                    }

                    // Reset header buffer for next time.
                    *filled = 0;

                    if payload_len == 0 {
                        frames.push(Frame {
                            msg_type,
                            stream_id,
                            payload: Vec::new(),
                        });
                        continue;
                    }

                    self.state = ParserState::Payload {
                        msg_type,
                        stream_id,
                        payload_len,
                        buf: Vec::with_capacity(payload_len),
                    };
                }
                ParserState::Payload {
                    msg_type,
                    stream_id,
                    payload_len,
                    buf,
                } => {
                    let need = payload_len.saturating_sub(buf.len());
                    let take = need.min(chunk.len());
                    buf.extend_from_slice(&chunk[..take]);
                    chunk = &chunk[take..];

                    if buf.len() < *payload_len {
                        continue;
                    }

                    let payload = core::mem::take(buf);
                    let msg_type = *msg_type;
                    let stream_id = *stream_id;
                    self.state = ParserState::Header {
                        buf: [0u8; TCP_MUX_HEADER_LEN],
                        filled: 0,
                    };
                    frames.push(Frame {
                        msg_type,
                        stream_id,
                        payload,
                    });
                }
            }
        }

        Ok(frames)
    }

    pub fn finish(&self) -> Result<(), Error> {
        match &self.state {
            ParserState::Header { filled, .. } => {
                if *filled == 0 {
                    Ok(())
                } else {
                    Err(Error::TruncatedStreamHeader { pending: *filled })
                }
            }
            ParserState::Payload {
                payload_len, buf, ..
            } => Err(Error::TruncatedStreamPayload {
                pending: buf.len(),
                payload_len: *payload_len,
            }),
        }
    }
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

pub fn encode_open_payload(
    host: &str,
    port: u16,
    metadata: Option<&str>,
) -> Result<Vec<u8>, Error> {
    let host_bytes = host.as_bytes();
    if host_bytes.len() > u16::MAX as usize {
        return Err(Error::OpenHostTooLong {
            len: host_bytes.len(),
            max: u16::MAX as usize,
        });
    }

    if port == 0 {
        return Err(Error::OpenInvalidPort { port });
    }

    let metadata_bytes = metadata.unwrap_or_default().as_bytes();
    if metadata_bytes.len() > u16::MAX as usize {
        return Err(Error::OpenMetadataTooLong {
            len: metadata_bytes.len(),
            max: u16::MAX as usize,
        });
    }

    let mut out = Vec::with_capacity(2 + host_bytes.len() + 2 + 2 + metadata_bytes.len());
    out.extend_from_slice(&(host_bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(host_bytes);
    out.extend_from_slice(&port.to_be_bytes());
    out.extend_from_slice(&(metadata_bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(metadata_bytes);
    Ok(out)
}

pub fn decode_open_payload(buf: &[u8]) -> Result<OpenPayload, Error> {
    // host_len (2) + port (2) + metadata_len (2)
    if buf.len() < 6 {
        return Err(Error::OpenPayloadTooShort { len: buf.len() });
    }

    let mut offset = 0;
    let host_len = u16::from_be_bytes([buf[offset], buf[offset + 1]]) as usize;
    offset += 2;

    if buf.len() < offset + host_len + 2 + 2 {
        return Err(Error::OpenPayloadTruncatedHost {
            host_len,
            remaining: buf.len().saturating_sub(offset),
        });
    }

    let host_bytes = &buf[offset..offset + host_len];
    offset += host_len;
    let host = core::str::from_utf8(host_bytes)
        .map_err(|_| Error::InvalidUtf8 {
            context: "OPEN host",
        })?
        .to_owned();

    let port = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
    offset += 2;

    let metadata_len = u16::from_be_bytes([buf[offset], buf[offset + 1]]) as usize;
    offset += 2;

    if buf.len() < offset + metadata_len {
        return Err(Error::OpenPayloadTruncatedMetadata {
            metadata_len,
            remaining: buf.len().saturating_sub(offset),
        });
    }

    let metadata_bytes = &buf[offset..offset + metadata_len];
    offset += metadata_len;
    let metadata = if metadata_len == 0 {
        None
    } else {
        Some(
            core::str::from_utf8(metadata_bytes)
                .map_err(|_| Error::InvalidUtf8 {
                    context: "OPEN metadata",
                })?
                .to_owned(),
        )
    };

    if offset != buf.len() {
        return Err(Error::OpenPayloadTrailingBytes {
            trailing: buf.len() - offset,
        });
    }

    Ok(OpenPayload {
        host,
        port,
        metadata,
    })
}

pub fn encode_close_payload(flags: u8) -> Vec<u8> {
    vec![flags]
}

pub fn decode_close_payload(buf: &[u8]) -> Result<u8, Error> {
    if buf.len() != 1 {
        return Err(Error::ClosePayloadWrongLen { len: buf.len() });
    }
    Ok(buf[0])
}

pub fn encode_error_payload(code: u16, message: &str) -> Result<Vec<u8>, Error> {
    let msg_bytes = message.as_bytes();
    if msg_bytes.len() > u16::MAX as usize {
        return Err(Error::ErrorMessageTooLong {
            len: msg_bytes.len(),
            max: u16::MAX as usize,
        });
    }

    let mut out = Vec::with_capacity(4 + msg_bytes.len());
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&(msg_bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(msg_bytes);
    Ok(out)
}

pub fn decode_error_payload(buf: &[u8]) -> Result<ErrorPayload, Error> {
    if buf.len() < 4 {
        return Err(Error::ErrorPayloadTooShort { len: buf.len() });
    }

    let code = u16::from_be_bytes([buf[0], buf[1]]);
    let msg_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let expected = 4usize
        .checked_add(msg_len)
        .ok_or(Error::ErrorPayloadLengthMismatch {
            expected: usize::MAX,
            got: buf.len(),
        })?;
    if buf.len() != expected {
        return Err(Error::ErrorPayloadLengthMismatch {
            expected,
            got: buf.len(),
        });
    }

    let msg = core::str::from_utf8(&buf[4..])
        .map_err(|_| Error::InvalidUtf8 {
            context: "ERROR message",
        })?
        .to_owned();

    Ok(ErrorPayload { code, message: msg })
}
