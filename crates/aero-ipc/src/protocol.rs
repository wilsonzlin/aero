//! Binary IPC message protocol.
//!
//! This is a deliberately small, stable format intended to be:
//! - zero-copy-ish (caller provides/receives byte buffers)
//! - endian-stable (little-endian)
//! - easy to implement in both Rust and TypeScript
//!
//! Records are framed by the ring buffer; this protocol defines the payload.

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// No-op, used for benchmarking / wakeups.
    Nop { seq: u32 },

    /// Request the worker to stop.
    Shutdown,

    /// MMIO read request. The worker should respond with an [`Event::MmioReadResp`].
    MmioRead { id: u32, addr: u64, size: u32 },

    /// MMIO write request. `data.len()` is the write size.
    MmioWrite { id: u32, addr: u64, data: Vec<u8> },

    /// Port I/O read request. The worker should respond with an
    /// [`Event::PortReadResp`].
    ///
    /// `size` is the access size in bytes (1/2/4).
    PortRead { id: u32, port: u16, size: u32 },

    /// Port I/O write request. The worker should respond with an
    /// [`Event::PortWriteResp`].
    ///
    /// `size` is the access size in bytes (1/2/4).
    PortWrite {
        id: u32,
        port: u16,
        size: u32,
        value: u32,
    },

    /// Disk read request.
    ///
    /// Read `len` bytes starting at `disk_offset` into guest memory at `guest_offset`.
    ///
    /// `guest_offset` is a **guest physical address** (GPA). Depending on the platform, guest RAM
    /// may be non-contiguous (e.g. PC/Q35 ECAM/PCI hole + high-RAM remap above 4 GiB), so the
    /// implementation must translate GPAs back into its backing store before indexing a flat
    /// byte buffer.
    DiskRead {
        id: u32,
        disk_offset: u64,
        len: u32,
        guest_offset: u64,
    },

    /// Disk write request.
    ///
    /// Write `len` bytes from guest memory at `guest_offset` to the disk starting at `disk_offset`.
    ///
    /// `guest_offset` is a **guest physical address** (GPA). See [`Command::DiskRead`] for notes on
    /// non-contiguous guest RAM layouts.
    DiskWrite {
        id: u32,
        disk_offset: u64,
        len: u32,
        guest_offset: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Pong response to [`Command::Nop`].
    Ack {
        seq: u32,
    },

    /// MMIO read response.
    MmioReadResp {
        id: u32,
        data: Vec<u8>,
    },

    /// Port I/O read response.
    ///
    /// The CPU side is expected to mask/truncate `value` based on the original
    /// `size` requested.
    PortReadResp {
        id: u32,
        value: u32,
    },

    /// MMIO write completion response.
    MmioWriteResp {
        id: u32,
    },

    /// Port I/O write completion response.
    PortWriteResp {
        id: u32,
    },

    /// Disk read completion response.
    ///
    /// If `ok` is false, `error_code` is set to an application-defined u32 code.
    DiskReadResp {
        id: u32,
        ok: bool,
        bytes: u32,
        error_code: Option<u32>,
    },

    /// Disk write completion response.
    ///
    /// If `ok` is false, `error_code` is set to an application-defined u32 code.
    DiskWriteResp {
        id: u32,
        ok: bool,
        bytes: u32,
        error_code: Option<u32>,
    },

    /// Frame completed and ready for presentation.
    FrameReady {
        frame_id: u64,
    },

    /// IRQ line changed.
    IrqRaise {
        irq: u8,
    },
    IrqLower {
        irq: u8,
    },

    /// System A20 gate line changed (typically via i8042 output port).
    A20Set {
        enabled: bool,
    },

    /// System reset requested (typically via i8042 output port / reset pulse).
    ResetRequest,

    /// Structured log record (UTF-8).
    Log {
        level: LogLevel,
        message: String,
    },

    /// Bytes written to a legacy serial port (16550).
    SerialOutput {
        port: u16,
        data: Vec<u8>,
    },

    /// Worker encountered a fatal error (panic or triple fault).
    Panic {
        message: String,
    },
    TripleFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn to_u8(self) -> u8 {
        match self {
            LogLevel::Trace => 0,
            LogLevel::Debug => 1,
            LogLevel::Info => 2,
            LogLevel::Warn => 3,
            LogLevel::Error => 4,
        }
    }

    fn from_u8(v: u8) -> Result<Self, DecodeError> {
        Ok(match v {
            0 => LogLevel::Trace,
            1 => LogLevel::Debug,
            2 => LogLevel::Info,
            3 => LogLevel::Warn,
            4 => LogLevel::Error,
            _ => return Err(DecodeError::InvalidEnum),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    UnexpectedEof,
    InvalidEnum,
    InvalidUtf8,
    UnknownTag,
    OversizedPayload,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEof => write!(f, "unexpected EOF"),
            DecodeError::InvalidEnum => write!(f, "invalid enum value"),
            DecodeError::InvalidUtf8 => write!(f, "invalid UTF-8"),
            DecodeError::UnknownTag => write!(f, "unknown tag"),
            DecodeError::OversizedPayload => write!(f, "payload too large"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Defensive maximum message size (bytes) for decode.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20; // 1 MiB

const CMD_TAG_NOP: u16 = 0x0000;
const CMD_TAG_SHUTDOWN: u16 = 0x0001;
const CMD_TAG_MMIO_READ: u16 = 0x0100;
const CMD_TAG_MMIO_WRITE: u16 = 0x0101;
const CMD_TAG_PORT_READ: u16 = 0x0102;
const CMD_TAG_PORT_WRITE: u16 = 0x0103;
const CMD_TAG_DISK_READ: u16 = 0x0104;
const CMD_TAG_DISK_WRITE: u16 = 0x0105;

const EVT_TAG_ACK: u16 = 0x1000;
const EVT_TAG_MMIO_READ_RESP: u16 = 0x1100;
const EVT_TAG_PORT_READ_RESP: u16 = 0x1101;
const EVT_TAG_MMIO_WRITE_RESP: u16 = 0x1102;
const EVT_TAG_PORT_WRITE_RESP: u16 = 0x1103;
const EVT_TAG_DISK_READ_RESP: u16 = 0x1104;
const EVT_TAG_DISK_WRITE_RESP: u16 = 0x1105;
const EVT_TAG_FRAME_READY: u16 = 0x1200;
const EVT_TAG_IRQ_RAISE: u16 = 0x1300;
const EVT_TAG_IRQ_LOWER: u16 = 0x1301;
const EVT_TAG_A20_SET: u16 = 0x1302;
const EVT_TAG_RESET_REQUEST: u16 = 0x1303;
const EVT_TAG_LOG: u16 = 0x1400;
const EVT_TAG_SERIAL_OUTPUT: u16 = 0x1500;
const EVT_TAG_PANIC: u16 = 0x1FFE;
const EVT_TAG_TRIPLE_FAULT: u16 = 0x1FFF;

pub fn encode_command(cmd: &Command) -> Vec<u8> {
    let mut out = Vec::new();
    encode_command_into(cmd, &mut out);
    out
}

pub fn encode_event(evt: &Event) -> Vec<u8> {
    let mut out = Vec::new();
    encode_event_into(evt, &mut out);
    out
}

pub fn encode_command_into(cmd: &Command, out: &mut Vec<u8>) {
    match cmd {
        Command::Nop { seq } => {
            push_u16(out, CMD_TAG_NOP);
            push_u32(out, *seq);
        }
        Command::Shutdown => {
            push_u16(out, CMD_TAG_SHUTDOWN);
        }
        Command::MmioRead { id, addr, size } => {
            push_u16(out, CMD_TAG_MMIO_READ);
            push_u32(out, *id);
            push_u64(out, *addr);
            push_u32(out, *size);
        }
        Command::MmioWrite { id, addr, data } => {
            push_u16(out, CMD_TAG_MMIO_WRITE);
            push_u32(out, *id);
            push_u64(out, *addr);
            push_u32(out, data.len() as u32);
            out.extend_from_slice(data);
        }
        Command::PortRead { id, port, size } => {
            push_u16(out, CMD_TAG_PORT_READ);
            push_u32(out, *id);
            push_u16(out, *port);
            push_u32(out, *size);
        }
        Command::PortWrite {
            id,
            port,
            size,
            value,
        } => {
            push_u16(out, CMD_TAG_PORT_WRITE);
            push_u32(out, *id);
            push_u16(out, *port);
            push_u32(out, *size);
            push_u32(out, *value);
        }
        Command::DiskRead {
            id,
            disk_offset,
            len,
            guest_offset,
        } => {
            push_u16(out, CMD_TAG_DISK_READ);
            push_u32(out, *id);
            push_u64(out, *disk_offset);
            push_u32(out, *len);
            push_u64(out, *guest_offset);
        }
        Command::DiskWrite {
            id,
            disk_offset,
            len,
            guest_offset,
        } => {
            push_u16(out, CMD_TAG_DISK_WRITE);
            push_u32(out, *id);
            push_u64(out, *disk_offset);
            push_u32(out, *len);
            push_u64(out, *guest_offset);
        }
    }
}

pub fn encode_event_into(evt: &Event, out: &mut Vec<u8>) {
    match evt {
        Event::Ack { seq } => {
            push_u16(out, EVT_TAG_ACK);
            push_u32(out, *seq);
        }
        Event::MmioReadResp { id, data } => {
            push_u16(out, EVT_TAG_MMIO_READ_RESP);
            push_u32(out, *id);
            push_u32(out, data.len() as u32);
            out.extend_from_slice(data);
        }
        Event::PortReadResp { id, value } => {
            push_u16(out, EVT_TAG_PORT_READ_RESP);
            push_u32(out, *id);
            push_u32(out, *value);
        }
        Event::MmioWriteResp { id } => {
            push_u16(out, EVT_TAG_MMIO_WRITE_RESP);
            push_u32(out, *id);
        }
        Event::PortWriteResp { id } => {
            push_u16(out, EVT_TAG_PORT_WRITE_RESP);
            push_u32(out, *id);
        }
        Event::DiskReadResp {
            id,
            ok,
            bytes,
            error_code,
        } => {
            push_u16(out, EVT_TAG_DISK_READ_RESP);
            push_u32(out, *id);
            out.push(if *ok { 1 } else { 0 });
            push_u32(out, *bytes);
            if !*ok {
                push_u32(out, error_code.unwrap_or(0));
            }
        }
        Event::DiskWriteResp {
            id,
            ok,
            bytes,
            error_code,
        } => {
            push_u16(out, EVT_TAG_DISK_WRITE_RESP);
            push_u32(out, *id);
            out.push(if *ok { 1 } else { 0 });
            push_u32(out, *bytes);
            if !*ok {
                push_u32(out, error_code.unwrap_or(0));
            }
        }
        Event::FrameReady { frame_id } => {
            push_u16(out, EVT_TAG_FRAME_READY);
            push_u64(out, *frame_id);
        }
        Event::IrqRaise { irq } => {
            push_u16(out, EVT_TAG_IRQ_RAISE);
            out.push(*irq);
        }
        Event::IrqLower { irq } => {
            push_u16(out, EVT_TAG_IRQ_LOWER);
            out.push(*irq);
        }
        Event::A20Set { enabled } => {
            push_u16(out, EVT_TAG_A20_SET);
            out.push(if *enabled { 1 } else { 0 });
        }
        Event::ResetRequest => {
            push_u16(out, EVT_TAG_RESET_REQUEST);
        }
        Event::Log { level, message } => {
            push_u16(out, EVT_TAG_LOG);
            out.push(level.to_u8());
            let msg = message.as_bytes();
            push_u32(out, msg.len() as u32);
            out.extend_from_slice(msg);
        }
        Event::SerialOutput { port, data } => {
            push_u16(out, EVT_TAG_SERIAL_OUTPUT);
            push_u16(out, *port);
            push_u32(out, data.len() as u32);
            out.extend_from_slice(data);
        }
        Event::Panic { message } => {
            push_u16(out, EVT_TAG_PANIC);
            let msg = message.as_bytes();
            push_u32(out, msg.len() as u32);
            out.extend_from_slice(msg);
        }
        Event::TripleFault => {
            push_u16(out, EVT_TAG_TRIPLE_FAULT);
        }
    }
}

pub fn decode_command(bytes: &[u8]) -> Result<Command, DecodeError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::OversizedPayload);
    }
    let mut r = Reader::new(bytes);
    let tag = r.read_u16()?;
    let cmd = match tag {
        CMD_TAG_NOP => Command::Nop { seq: r.read_u32()? },
        CMD_TAG_SHUTDOWN => Command::Shutdown,
        CMD_TAG_MMIO_READ => Command::MmioRead {
            id: r.read_u32()?,
            addr: r.read_u64()?,
            size: r.read_u32()?,
        },
        CMD_TAG_MMIO_WRITE => {
            let id = r.read_u32()?;
            let addr = r.read_u64()?;
            let len = r.read_u32()? as usize;
            let data = r.read_bytes(len)?.to_vec();
            Command::MmioWrite { id, addr, data }
        }
        CMD_TAG_PORT_READ => Command::PortRead {
            id: r.read_u32()?,
            port: r.read_u16()?,
            size: r.read_u32()?,
        },
        CMD_TAG_PORT_WRITE => Command::PortWrite {
            id: r.read_u32()?,
            port: r.read_u16()?,
            size: r.read_u32()?,
            value: r.read_u32()?,
        },
        CMD_TAG_DISK_READ => Command::DiskRead {
            id: r.read_u32()?,
            disk_offset: r.read_u64()?,
            len: r.read_u32()?,
            guest_offset: r.read_u64()?,
        },
        CMD_TAG_DISK_WRITE => Command::DiskWrite {
            id: r.read_u32()?,
            disk_offset: r.read_u64()?,
            len: r.read_u32()?,
            guest_offset: r.read_u64()?,
        },
        _ => return Err(DecodeError::UnknownTag),
    };
    if r.remaining() != 0 {
        // Extra bytes are considered a format violation.
        return Err(DecodeError::UnknownTag);
    }
    Ok(cmd)
}

pub fn decode_event(bytes: &[u8]) -> Result<Event, DecodeError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::OversizedPayload);
    }
    let mut r = Reader::new(bytes);
    let tag = r.read_u16()?;
    let evt = match tag {
        EVT_TAG_ACK => Event::Ack { seq: r.read_u32()? },
        EVT_TAG_MMIO_READ_RESP => {
            let id = r.read_u32()?;
            let len = r.read_u32()? as usize;
            let data = r.read_bytes(len)?.to_vec();
            Event::MmioReadResp { id, data }
        }
        EVT_TAG_PORT_READ_RESP => Event::PortReadResp {
            id: r.read_u32()?,
            value: r.read_u32()?,
        },
        EVT_TAG_MMIO_WRITE_RESP => Event::MmioWriteResp { id: r.read_u32()? },
        EVT_TAG_PORT_WRITE_RESP => Event::PortWriteResp { id: r.read_u32()? },
        EVT_TAG_DISK_READ_RESP => {
            let id = r.read_u32()?;
            let ok = r.read_u8()? != 0;
            let bytes = r.read_u32()?;
            let error_code = if ok { None } else { Some(r.read_u32()?) };
            Event::DiskReadResp {
                id,
                ok,
                bytes,
                error_code,
            }
        }
        EVT_TAG_DISK_WRITE_RESP => {
            let id = r.read_u32()?;
            let ok = r.read_u8()? != 0;
            let bytes = r.read_u32()?;
            let error_code = if ok { None } else { Some(r.read_u32()?) };
            Event::DiskWriteResp {
                id,
                ok,
                bytes,
                error_code,
            }
        }
        EVT_TAG_FRAME_READY => Event::FrameReady {
            frame_id: r.read_u64()?,
        },
        EVT_TAG_IRQ_RAISE => Event::IrqRaise { irq: r.read_u8()? },
        EVT_TAG_IRQ_LOWER => Event::IrqLower { irq: r.read_u8()? },
        EVT_TAG_A20_SET => Event::A20Set {
            enabled: r.read_u8()? != 0,
        },
        EVT_TAG_RESET_REQUEST => Event::ResetRequest,
        EVT_TAG_LOG => {
            let level = LogLevel::from_u8(r.read_u8()?)?;
            let len = r.read_u32()? as usize;
            let msg = r.read_bytes(len)?;
            let message = core::str::from_utf8(msg).map_err(|_| DecodeError::InvalidUtf8)?;
            Event::Log {
                level,
                message: message.to_string(),
            }
        }
        EVT_TAG_SERIAL_OUTPUT => {
            let port = r.read_u16()?;
            let len = r.read_u32()? as usize;
            let data = r.read_bytes(len)?.to_vec();
            Event::SerialOutput { port, data }
        }
        EVT_TAG_PANIC => {
            let len = r.read_u32()? as usize;
            let msg = r.read_bytes(len)?;
            let message = core::str::from_utf8(msg).map_err(|_| DecodeError::InvalidUtf8)?;
            Event::Panic {
                message: message.to_string(),
            }
        }
        EVT_TAG_TRIPLE_FAULT => Event::TripleFault,
        _ => return Err(DecodeError::UnknownTag),
    };
    if r.remaining() != 0 {
        return Err(DecodeError::UnknownTag);
    }
    Ok(evt)
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let b = *self.bytes.get(self.pos).ok_or(DecodeError::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < len {
            return Err(DecodeError::UnexpectedEof);
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.bytes[start..start + len])
    }
}
