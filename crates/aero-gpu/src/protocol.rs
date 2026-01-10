//! AeroGPU Guest↔Host command stream protocol (host-side parser).
//!
//! This module mirrors the C ABI defined in `drivers/aerogpu/protocol/aerogpu_cmd.h`.
//! The host consumes a byte slice containing:
//! - `aerogpu_cmd_stream_header`
//! - a sequence of command packets, each starting with `aerogpu_cmd_hdr`
//!
//! The parser is intentionally conservative:
//! - validates sizes and alignment
//! - skips unknown opcodes using `size_bytes`
//! - never performs unaligned reads into `repr(C)` structs
//!
//! This allows the protocol to be consumed safely from guest-provided memory.

use core::fmt;

pub const AEROGPU_CMD_STREAM_MAGIC: u32 = 0x444D_4341; // "ACMD" little-endian

const STREAM_HEADER_SIZE: usize = 24;
const CMD_HDR_SIZE: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuCmdStreamHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub flags: u32,
}

impl AeroGpuCmdStreamHeader {
    pub fn is_magic_valid(&self) -> bool {
        self.magic == AEROGPU_CMD_STREAM_MAGIC
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeroGpuCmdHdr {
    pub opcode: u32,
    pub size_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AeroGpuOpcode {
    Nop = 0,
    DebugMarker = 1,

    // Presentation
    Present = 0x700,
    PresentEx = 0x701,

    // D3D9Ex/DWM shared surface interop.
    ExportSharedSurface = 0x710,
    ImportSharedSurface = 0x711,

    // Explicit flush.
    Flush = 0x720,
}

impl AeroGpuOpcode {
    fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            x if x == AeroGpuOpcode::Nop as u32 => AeroGpuOpcode::Nop,
            x if x == AeroGpuOpcode::DebugMarker as u32 => AeroGpuOpcode::DebugMarker,
            x if x == AeroGpuOpcode::Present as u32 => AeroGpuOpcode::Present,
            x if x == AeroGpuOpcode::PresentEx as u32 => AeroGpuOpcode::PresentEx,
            x if x == AeroGpuOpcode::ExportSharedSurface as u32 => {
                AeroGpuOpcode::ExportSharedSurface
            }
            x if x == AeroGpuOpcode::ImportSharedSurface as u32 => {
                AeroGpuOpcode::ImportSharedSurface
            }
            x if x == AeroGpuOpcode::Flush as u32 => AeroGpuOpcode::Flush,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AeroGpuCmd<'a> {
    Nop,
    DebugMarker {
        bytes: &'a [u8],
    },

    Present {
        scanout_id: u32,
        flags: u32,
    },
    PresentEx {
        scanout_id: u32,
        flags: u32,
        d3d9_present_flags: u32,
    },

    ExportSharedSurface {
        resource_handle: u32,
        share_token: u64,
    },
    ImportSharedSurface {
        out_resource_handle: u32,
        share_token: u64,
    },

    Flush,

    /// Unrecognized opcode; payload is the bytes after `AeroGpuCmdHdr`.
    Unknown {
        opcode: u32,
        payload: &'a [u8],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AeroGpuCmdStreamParseError {
    BufferTooSmall,
    InvalidMagic(u32),
    InvalidSizeBytes { size_bytes: u32, buffer_len: usize },
    InvalidCmdSizeBytes(u32),
    MisalignedCmdSizeBytes(u32),
}

impl fmt::Display for AeroGpuCmdStreamParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AeroGpuCmdStreamParseError::BufferTooSmall => write!(f, "buffer too small"),
            AeroGpuCmdStreamParseError::InvalidMagic(magic) => {
                write!(f, "invalid command stream magic 0x{magic:08X}")
            }
            AeroGpuCmdStreamParseError::InvalidSizeBytes {
                size_bytes,
                buffer_len,
            } => write!(
                f,
                "invalid command stream size_bytes={size_bytes} (buffer_len={buffer_len})"
            ),
            AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(size) => {
                write!(f, "invalid command packet size_bytes={size}")
            }
            AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(size) => {
                write!(f, "command packet size_bytes={size} is not 4-byte aligned")
            }
        }
    }
}

impl std::error::Error for AeroGpuCmdStreamParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AeroGpuCmdStreamView<'a> {
    pub header: AeroGpuCmdStreamHeader,
    pub cmds: Vec<AeroGpuCmd<'a>>,
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

pub fn parse_cmd_stream(
    bytes: &[u8],
) -> Result<AeroGpuCmdStreamView<'_>, AeroGpuCmdStreamParseError> {
    if bytes.len() < STREAM_HEADER_SIZE {
        return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
    }

    let magic = read_u32_le(&bytes[0..4]);
    if magic != AEROGPU_CMD_STREAM_MAGIC {
        return Err(AeroGpuCmdStreamParseError::InvalidMagic(magic));
    }
    let abi_version = read_u32_le(&bytes[4..8]);
    let size_bytes = read_u32_le(&bytes[8..12]);
    let flags = read_u32_le(&bytes[12..16]);

    let header = AeroGpuCmdStreamHeader {
        magic,
        abi_version,
        size_bytes,
        flags,
    };

    let size_bytes_usize = size_bytes as usize;
    if size_bytes_usize < STREAM_HEADER_SIZE || size_bytes_usize > bytes.len() {
        return Err(AeroGpuCmdStreamParseError::InvalidSizeBytes {
            size_bytes,
            buffer_len: bytes.len(),
        });
    }

    let mut cmds = Vec::new();
    let mut offset = STREAM_HEADER_SIZE;
    while offset < size_bytes_usize {
        if offset + CMD_HDR_SIZE > size_bytes_usize {
            return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
        }
        let opcode = read_u32_le(&bytes[offset..offset + 4]);
        let cmd_size_bytes = read_u32_le(&bytes[offset + 4..offset + 8]);
        if cmd_size_bytes < CMD_HDR_SIZE as u32 {
            return Err(AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(
                cmd_size_bytes,
            ));
        }
        if cmd_size_bytes % 4 != 0 {
            return Err(AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(
                cmd_size_bytes,
            ));
        }
        let cmd_size_usize = cmd_size_bytes as usize;
        let end = offset + cmd_size_usize;
        if end > size_bytes_usize {
            return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
        }

        let payload = &bytes[offset + CMD_HDR_SIZE..end];
        let cmd = match AeroGpuOpcode::from_u32(opcode) {
            Some(AeroGpuOpcode::Nop) => AeroGpuCmd::Nop,
            Some(AeroGpuOpcode::DebugMarker) => AeroGpuCmd::DebugMarker { bytes: payload },
            Some(AeroGpuOpcode::Present) => {
                if payload.len() < 8 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let scanout_id = read_u32_le(&payload[0..4]);
                let flags = read_u32_le(&payload[4..8]);
                AeroGpuCmd::Present { scanout_id, flags }
            }
            Some(AeroGpuOpcode::PresentEx) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let scanout_id = read_u32_le(&payload[0..4]);
                let flags = read_u32_le(&payload[4..8]);
                let d3d9_present_flags = read_u32_le(&payload[8..12]);
                AeroGpuCmd::PresentEx {
                    scanout_id,
                    flags,
                    d3d9_present_flags,
                }
            }
            Some(AeroGpuOpcode::ExportSharedSurface) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let resource_handle = read_u32_le(&payload[0..4]);
                let share_token = read_u64_le(&payload[8..16]);
                AeroGpuCmd::ExportSharedSurface {
                    resource_handle,
                    share_token,
                }
            }
            Some(AeroGpuOpcode::ImportSharedSurface) => {
                if payload.len() < 16 {
                    return Err(AeroGpuCmdStreamParseError::BufferTooSmall);
                }
                let out_resource_handle = read_u32_le(&payload[0..4]);
                let share_token = read_u64_le(&payload[8..16]);
                AeroGpuCmd::ImportSharedSurface {
                    out_resource_handle,
                    share_token,
                }
            }
            Some(AeroGpuOpcode::Flush) => AeroGpuCmd::Flush,
            None => AeroGpuCmd::Unknown { opcode, payload },
        };

        cmds.push(cmd);
        offset = end;
    }

    Ok(AeroGpuCmdStreamView { header, cmds })
}
