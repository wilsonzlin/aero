//! Guest → host GPU command ABI.
//!
//! The real emulator will likely feed this via a virtual GPU device/driver.
//! For now we keep the ABI simple and easy to parse from a byte slice.

use thiserror::Error;

/// All commands are little-endian and start with this header.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandHeader {
    /// `Opcode::* as u32`.
    pub opcode: u32,
    /// Total command size in 32-bit words, including the header.
    pub size_words: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Opcode {
    SetVertexShader = 1,
    SetPixelShader = 2,
    SetVertexDecl = 3,
    SetRenderState = 4,
    SetSamplerState = 5,
    SetTexture = 6,
    DrawIndexed = 7,
    Draw = 8,
    Present = 9,
}

#[derive(Debug, Error)]
pub enum CommandParseError {
    #[error("command buffer too small")]
    BufferTooSmall,
    #[error("invalid command header size_words={0}")]
    InvalidSize(u32),
    #[error("unknown opcode {0}")]
    UnknownOpcode(u32),
}

/// A parsed, strongly-typed command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command<'a> {
    SetVertexShader {
        dxbc: &'a [u8],
    },
    SetPixelShader {
        dxbc: &'a [u8],
    },
    /// Raw bytes containing a serialized `D3DVERTEXELEMENT9[]`.
    ///
    /// This is expected to be a stream of 8-byte structs (little-endian), terminated by the
    /// standard D3D9 end marker (`stream=0xFF, type=UNUSED`).
    ///
    /// See [`crate::vertex::VertexDeclaration::from_d3d_bytes`].
    SetVertexDecl {
        bytes: &'a [u8],
    },
    /// Packed `u32` key/value pairs (little-endian).
    ///
    /// Keeping this as raw bytes avoids alignment issues when parsing from a
    /// guest-provided buffer.
    SetRenderState {
        states: &'a [u8],
    },
    /// Packed `u32` key/value pairs (little-endian).
    SetSamplerState {
        sampler: u32,
        states: &'a [u8],
    },
    /// Bind a texture handle to a sampler slot.
    SetTexture {
        sampler: u32,
        texture_handle: u64,
    },
    DrawIndexed {
        index_count: u32,
        start_index: u32,
        base_vertex: i32,
    },
    Draw {
        vertex_count: u32,
        start_vertex: u32,
    },
    Present,
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

/// Parse a stream of commands from a byte slice.
pub fn parse_commands(mut bytes: &[u8]) -> Result<Vec<Command<'_>>, CommandParseError> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 8 {
            return Err(CommandParseError::BufferTooSmall);
        }
        let opcode = read_u32_le(&bytes[0..4]);
        let size_words = read_u32_le(&bytes[4..8]);
        if size_words < 2 {
            return Err(CommandParseError::InvalidSize(size_words));
        }
        let size_bytes = (size_words as usize) * 4;
        if bytes.len() < size_bytes {
            return Err(CommandParseError::BufferTooSmall);
        }
        let payload = &bytes[8..size_bytes];
        let cmd = match opcode {
            x if x == Opcode::SetVertexShader as u32 => Command::SetVertexShader { dxbc: payload },
            x if x == Opcode::SetPixelShader as u32 => Command::SetPixelShader { dxbc: payload },
            x if x == Opcode::SetVertexDecl as u32 => Command::SetVertexDecl { bytes: payload },
            x if x == Opcode::SetRenderState as u32 => {
                if payload.len() % 4 != 0 {
                    return Err(CommandParseError::BufferTooSmall);
                }
                Command::SetRenderState { states: payload }
            }
            x if x == Opcode::SetSamplerState as u32 => {
                if payload.len() < 4 {
                    return Err(CommandParseError::BufferTooSmall);
                }
                let sampler = read_u32_le(&payload[0..4]);
                if (payload.len() - 4) % 4 != 0 {
                    return Err(CommandParseError::BufferTooSmall);
                }
                Command::SetSamplerState {
                    sampler,
                    states: &payload[4..],
                }
            }
            x if x == Opcode::SetTexture as u32 => {
                if payload.len() < 12 {
                    return Err(CommandParseError::BufferTooSmall);
                }
                let sampler = read_u32_le(&payload[0..4]);
                let texture_handle = read_u64_le(&payload[4..12]);
                Command::SetTexture {
                    sampler,
                    texture_handle,
                }
            }
            x if x == Opcode::DrawIndexed as u32 => {
                if payload.len() < 12 {
                    return Err(CommandParseError::BufferTooSmall);
                }
                let index_count = read_u32_le(&payload[0..4]);
                let start_index = read_u32_le(&payload[4..8]);
                let base_vertex = read_u32_le(&payload[8..12]) as i32;
                Command::DrawIndexed {
                    index_count,
                    start_index,
                    base_vertex,
                }
            }
            x if x == Opcode::Draw as u32 => {
                if payload.len() < 8 {
                    return Err(CommandParseError::BufferTooSmall);
                }
                let vertex_count = read_u32_le(&payload[0..4]);
                let start_vertex = read_u32_le(&payload[4..8]);
                Command::Draw {
                    vertex_count,
                    start_vertex,
                }
            }
            x if x == Opcode::Present as u32 => Command::Present,
            other => return Err(CommandParseError::UnknownOpcode(other)),
        };
        out.push(cmd);
        bytes = &bytes[size_bytes..];
    }
    Ok(out)
}
