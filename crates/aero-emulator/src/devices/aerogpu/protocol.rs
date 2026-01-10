//! AeroGPU command ABI definitions.
//!
//! All fields are little-endian and naturally aligned, with additional 8-byte alignment for
//! command and event entries in rings.

use std::fmt;

pub const ABI_VERSION_MAJOR: u16 = 0;
pub const ABI_VERSION_MINOR: u16 = 1;
pub const ABI_VERSION_PATCH: u16 = 0;

pub const RING_ALIGNMENT: usize = 8;

/// Shared capability bits.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub caps_bits: u32,
    pub max_surface_width: u32,
    pub max_surface_height: u32,
    pub max_surfaces: u32,
}

impl Caps {
    pub const CAPS_EVENT_RING: u32 = 1 << 0;
    pub const CAPS_FORMAT_RGBA8888: u32 = 1 << 1;

    pub fn default_caps() -> Self {
        Self {
            caps_bits: Self::CAPS_EVENT_RING | Self::CAPS_FORMAT_RGBA8888,
            max_surface_width: 4096,
            max_surface_height: 4096,
            max_surfaces: 1024,
        }
    }

    pub fn abi_version_u32(&self) -> u32 {
        ((ABI_VERSION_MAJOR as u32) << 16)
            | ((ABI_VERSION_MINOR as u32) << 8)
            | (ABI_VERSION_PATCH as u32)
    }

    pub const SIZE_BYTES: usize = 16;

    /// Encode the shared capabilities struct (little-endian).
    ///
    /// This is intended for placing into a shared memory region (e.g. a PCI BAR or fixed MMIO
    /// mapping). The fields are:
    ///
    /// - `caps_bits`
    /// - `max_surface_width`
    /// - `max_surface_height`
    /// - `max_surfaces`
    pub fn encode(self) -> [u8; Self::SIZE_BYTES] {
        let mut out = [0u8; Self::SIZE_BYTES];
        out[0..4].copy_from_slice(&self.caps_bits.to_le_bytes());
        out[4..8].copy_from_slice(&self.max_surface_width.to_le_bytes());
        out[8..12].copy_from_slice(&self.max_surface_height.to_le_bytes());
        out[12..16].copy_from_slice(&self.max_surfaces.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE_BYTES {
            return None;
        }
        Some(Self {
            caps_bits: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            max_surface_width: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            max_surface_height: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            max_surfaces: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
        })
    }
}

/// Command opcodes.
pub struct Opcode;

impl Opcode {
    pub const NOP: u32 = 0;
    pub const CREATE_SURFACE: u32 = 1;
    pub const UPDATE_SURFACE: u32 = 2;
    pub const CLEAR_RGBA: u32 = 3;
    pub const DRAW_TRIANGLE_TEST: u32 = 4;
    pub const PRESENT: u32 = 5;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SurfaceFormat {
    Rgba8888 = 1,
}

impl SurfaceFormat {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Rgba8888),
            _ => None,
        }
    }

    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8888 => 4,
        }
    }
}

impl Default for SurfaceFormat {
    fn default() -> Self {
        Self::Rgba8888
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum StatusCode {
    Ok = 0,
    InvalidOpcode = 1,
    InvalidSize = 2,
    InvalidArgument = 3,
    SurfaceNotFound = 4,
    UnsupportedFormat = 5,
    GuestMemoryFault = 6,
    OutOfMemory = 7,
}

impl fmt::Debug for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Ok => "OK",
            Self::InvalidOpcode => "INVALID_OPCODE",
            Self::InvalidSize => "INVALID_SIZE",
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::SurfaceNotFound => "SURFACE_NOT_FOUND",
            Self::UnsupportedFormat => "UNSUPPORTED_FORMAT",
            Self::GuestMemoryFault => "GUEST_MEMORY_FAULT",
            Self::OutOfMemory => "OUT_OF_MEMORY",
        };
        f.write_str(s)
    }
}

/// Ring command header: `{ opcode: u32, size_bytes: u32 }`.
#[derive(Debug, Clone, Copy)]
pub struct CmdHeader {
    pub opcode: u32,
    pub size_bytes: u32,
}

impl CmdHeader {
    pub const SIZE_BYTES: usize = 8;

    pub fn encode(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..4].copy_from_slice(&self.opcode.to_le_bytes());
        out[4..8].copy_from_slice(&self.size_bytes.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE_BYTES {
            return None;
        }
        let opcode = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        let size_bytes = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
        if size_bytes as usize != bytes.len() {
            return None;
        }
        if size_bytes as usize % RING_ALIGNMENT != 0 {
            return None;
        }
        Some(Self { opcode, size_bytes })
    }
}

fn read_u32_le(payload: &[u8], offset: usize) -> Option<u32> {
    payload
        .get(offset..offset + 4)?
        .try_into()
        .ok()
        .map(u32::from_le_bytes)
}

fn read_u64_le(payload: &[u8], offset: usize) -> Option<u64> {
    payload
        .get(offset..offset + 8)?
        .try_into()
        .ok()
        .map(u64::from_le_bytes)
}

#[derive(Debug, Clone, Copy)]
pub struct CmdCreateSurface {
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

impl CmdCreateSurface {
    pub const PAYLOAD_SIZE: usize = 12;

    pub fn encode(self) -> Vec<u8> {
        let size_bytes = align_ring(CmdHeader::SIZE_BYTES + Self::PAYLOAD_SIZE) as u32;
        let mut out = vec![0u8; size_bytes as usize];
        out[0..8].copy_from_slice(
            &CmdHeader {
                opcode: Opcode::CREATE_SURFACE,
                size_bytes,
            }
            .encode(),
        );
        out[8..12].copy_from_slice(&self.width.to_le_bytes());
        out[12..16].copy_from_slice(&self.height.to_le_bytes());
        out[16..20].copy_from_slice(&self.format.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::PAYLOAD_SIZE {
            return None;
        }
        Some(Self {
            width: read_u32_le(payload, 0)?,
            height: read_u32_le(payload, 4)?,
            format: read_u32_le(payload, 8)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CmdUpdateSurface {
    pub surface_id: u32,
    pub guest_phys_addr: u64,
    pub stride: u32,
}

impl CmdUpdateSurface {
    pub const PAYLOAD_SIZE: usize = 16;

    pub fn encode(self) -> Vec<u8> {
        let size_bytes = align_ring(CmdHeader::SIZE_BYTES + Self::PAYLOAD_SIZE) as u32;
        let mut out = vec![0u8; size_bytes as usize];
        out[0..8].copy_from_slice(
            &CmdHeader {
                opcode: Opcode::UPDATE_SURFACE,
                size_bytes,
            }
            .encode(),
        );
        out[8..12].copy_from_slice(&self.surface_id.to_le_bytes());
        out[12..20].copy_from_slice(&self.guest_phys_addr.to_le_bytes());
        out[20..24].copy_from_slice(&self.stride.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::PAYLOAD_SIZE {
            return None;
        }
        Some(Self {
            surface_id: read_u32_le(payload, 0)?,
            guest_phys_addr: read_u64_le(payload, 4)?,
            stride: read_u32_le(payload, 12)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CmdClearRgba {
    pub surface_id: u32,
    /// Packed as `r | g<<8 | b<<16 | a<<24`.
    pub rgba: u32,
}

impl CmdClearRgba {
    pub const PAYLOAD_SIZE: usize = 8;

    pub fn encode(self) -> Vec<u8> {
        let size_bytes = align_ring(CmdHeader::SIZE_BYTES + Self::PAYLOAD_SIZE) as u32;
        let mut out = vec![0u8; size_bytes as usize];
        out[0..8].copy_from_slice(
            &CmdHeader {
                opcode: Opcode::CLEAR_RGBA,
                size_bytes,
            }
            .encode(),
        );
        out[8..12].copy_from_slice(&self.surface_id.to_le_bytes());
        out[12..16].copy_from_slice(&self.rgba.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::PAYLOAD_SIZE {
            return None;
        }
        Some(Self {
            surface_id: read_u32_le(payload, 0)?,
            rgba: read_u32_le(payload, 4)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CmdDrawTriangleTest {
    pub surface_id: u32,
}

impl CmdDrawTriangleTest {
    pub const PAYLOAD_SIZE: usize = 4;

    pub fn encode(self) -> Vec<u8> {
        let size_bytes = align_ring(CmdHeader::SIZE_BYTES + Self::PAYLOAD_SIZE) as u32;
        let mut out = vec![0u8; size_bytes as usize];
        out[0..8].copy_from_slice(
            &CmdHeader {
                opcode: Opcode::DRAW_TRIANGLE_TEST,
                size_bytes,
            }
            .encode(),
        );
        out[8..12].copy_from_slice(&self.surface_id.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::PAYLOAD_SIZE {
            return None;
        }
        Some(Self {
            surface_id: read_u32_le(payload, 0)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CmdPresent {
    pub surface_id: u32,
}

impl CmdPresent {
    pub const PAYLOAD_SIZE: usize = 4;

    pub fn encode(self) -> Vec<u8> {
        let size_bytes = align_ring(CmdHeader::SIZE_BYTES + Self::PAYLOAD_SIZE) as u32;
        let mut out = vec![0u8; size_bytes as usize];
        out[0..8].copy_from_slice(
            &CmdHeader {
                opcode: Opcode::PRESENT,
                size_bytes,
            }
            .encode(),
        );
        out[8..12].copy_from_slice(&self.surface_id.to_le_bytes());
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < Self::PAYLOAD_SIZE {
            return None;
        }
        Some(Self {
            surface_id: read_u32_le(payload, 0)?,
        })
    }
}

/// Event type codes.
pub struct EventType;

impl EventType {
    pub const CMD_STATUS: u32 = 1;
}

#[derive(Debug, Clone, Copy)]
pub struct EventHeader {
    pub event_type: u32,
    pub size_bytes: u32,
}

impl EventHeader {
    pub const SIZE_BYTES: usize = 8;

    pub fn encode(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..4].copy_from_slice(&self.event_type.to_le_bytes());
        out[4..8].copy_from_slice(&self.size_bytes.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE_BYTES {
            return None;
        }
        let event_type = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        let size_bytes = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
        if size_bytes as usize != bytes.len() {
            return None;
        }
        if size_bytes as usize % RING_ALIGNMENT != 0 {
            return None;
        }
        Some(Self {
            event_type,
            size_bytes,
        })
    }
}

/// Completion/error event for a command.
#[derive(Debug, Clone, Copy)]
pub struct EventCmdStatus {
    pub opcode: u32,
    pub status: StatusCode,
    pub data: [u32; 4],
}

impl EventCmdStatus {
    pub const PAYLOAD_SIZE: usize = 24;

    pub fn new(opcode: u32, status: StatusCode, data: [u32; 4]) -> Self {
        Self {
            opcode,
            status,
            data,
        }
    }

    pub fn encode(self) -> Vec<u8> {
        let size_bytes = align_ring(EventHeader::SIZE_BYTES + Self::PAYLOAD_SIZE) as u32;
        let mut out = vec![0u8; size_bytes as usize];
        out[0..8].copy_from_slice(
            &EventHeader {
                event_type: EventType::CMD_STATUS,
                size_bytes,
            }
            .encode(),
        );
        out[8..12].copy_from_slice(&self.opcode.to_le_bytes());
        out[12..16].copy_from_slice(&(self.status as u32).to_le_bytes());
        out[16..20].copy_from_slice(&self.data[0].to_le_bytes());
        out[20..24].copy_from_slice(&self.data[1].to_le_bytes());
        out[24..28].copy_from_slice(&self.data[2].to_le_bytes());
        out[28..32].copy_from_slice(&self.data[3].to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let hdr = EventHeader::decode(bytes)?;
        if hdr.event_type != EventType::CMD_STATUS {
            return None;
        }
        let payload = &bytes[EventHeader::SIZE_BYTES..];
        if payload.len() < Self::PAYLOAD_SIZE {
            return None;
        }
        let opcode = read_u32_le(payload, 0)?;
        let status_u32 = read_u32_le(payload, 4)?;
        let status = match status_u32 {
            0 => StatusCode::Ok,
            1 => StatusCode::InvalidOpcode,
            2 => StatusCode::InvalidSize,
            3 => StatusCode::InvalidArgument,
            4 => StatusCode::SurfaceNotFound,
            5 => StatusCode::UnsupportedFormat,
            6 => StatusCode::GuestMemoryFault,
            7 => StatusCode::OutOfMemory,
            _ => return None,
        };
        Some(Self {
            opcode,
            status,
            data: [
                read_u32_le(payload, 8)?,
                read_u32_le(payload, 12)?,
                read_u32_le(payload, 16)?,
                read_u32_le(payload, 20)?,
            ],
        })
    }
}

/// Interrupt status bits (exposed via `MMIO.IRQ_STATUS`).
pub struct IrqBits;

impl IrqBits {
    pub const CMD_PROCESSED: u32 = 1 << 0;
    pub const PRESENT_DONE: u32 = 1 << 1;
}

pub fn align_ring(size: usize) -> usize {
    let rem = size % RING_ALIGNMENT;
    if rem == 0 {
        size
    } else {
        size + (RING_ALIGNMENT - rem)
    }
}
