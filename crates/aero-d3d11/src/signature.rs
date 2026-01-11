//! Parsing and representation of DXBC signature chunks (`ISGN`/`OSGN`/`PSGN`).
//!
//! The signature chunks provide the semantic ↔ register mapping used by D3D10+
//! shaders. The WGSL translator uses them to generate vertex input/output
//! structs and to provide reflection for input layout construction.

use core::fmt;

use crate::{DxbcFile, FourCC};

/// Parsed signature table from an `ISGN`/`OSGN`/`PSGN` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcSignature {
    pub parameters: Vec<DxbcSignatureParameter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxbcSignatureParameter {
    pub semantic_name: String,
    pub semantic_index: u32,
    pub system_value_type: u32,
    pub component_type: u32,
    pub register: u32,
    pub mask: u8,
    pub read_write_mask: u8,
    pub stream: u8,
    pub min_precision: u8,
}

/// Collection of optional signature chunks found in a DXBC container.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShaderSignatures {
    pub isgn: Option<DxbcSignature>,
    pub osgn: Option<DxbcSignature>,
    pub psgn: Option<DxbcSignature>,
}

#[derive(Debug)]
pub enum SignatureError {
    MissingChunk(FourCC),
    MalformedChunk {
        fourcc: FourCC,
        reason: &'static str,
    },
    OutOfBounds {
        fourcc: FourCC,
        reason: &'static str,
    },
    InvalidUtf8 {
        fourcc: FourCC,
        reason: &'static str,
    },
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignatureError::MissingChunk(fourcc) => write!(f, "DXBC missing {fourcc} signature chunk"),
            SignatureError::MalformedChunk { fourcc, reason } => {
                write!(f, "malformed DXBC {fourcc} signature chunk: {reason}")
            }
            SignatureError::OutOfBounds { fourcc, reason } => {
                write!(f, "DXBC {fourcc} signature chunk out of bounds: {reason}")
            }
            SignatureError::InvalidUtf8 { fourcc, reason } => {
                write!(f, "DXBC {fourcc} signature chunk contains invalid UTF-8: {reason}")
            }
        }
    }
}

impl std::error::Error for SignatureError {}

pub fn parse_signatures(dxbc: &DxbcFile<'_>) -> Result<ShaderSignatures, SignatureError> {
    const ISGN: FourCC = FourCC(*b"ISGN");
    const OSGN: FourCC = FourCC(*b"OSGN");
    const PSGN: FourCC = FourCC(*b"PSGN");

    Ok(ShaderSignatures {
        isgn: dxbc.get_chunk(ISGN).map(|c| parse_signature_chunk(ISGN, c.data)).transpose()?,
        osgn: dxbc.get_chunk(OSGN).map(|c| parse_signature_chunk(OSGN, c.data)).transpose()?,
        psgn: dxbc.get_chunk(PSGN).map(|c| parse_signature_chunk(PSGN, c.data)).transpose()?,
    })
}

pub fn parse_signature_chunk(fourcc: FourCC, bytes: &[u8]) -> Result<DxbcSignature, SignatureError> {
    let mut r = Reader::new(bytes, fourcc);
    let param_count = r.read_u32_le()?;
    let param_offset = r.read_u32_le()?;

    let entry_size = 24usize;
    let table_bytes = (param_count as usize)
        .checked_mul(entry_size)
        .ok_or(SignatureError::MalformedChunk {
            fourcc,
            reason: "parameter count overflow",
        })?;

    let table_start = param_offset as usize;
    if table_start
        .checked_add(table_bytes)
        .is_none()
        || table_start + table_bytes > bytes.len()
    {
        return Err(SignatureError::OutOfBounds {
            fourcc,
            reason: "signature parameter table out of bounds",
        });
    }

    let mut parameters = Vec::with_capacity(param_count as usize);
    for i in 0..param_count {
        let offset = table_start + (i as usize) * entry_size;
        let mut pr = r.fork(offset)?;

        let semantic_name_offset = pr.read_u32_le()?;
        let semantic_index = pr.read_u32_le()?;
        let system_value_type = pr.read_u32_le()?;
        let component_type = pr.read_u32_le()?;
        let register = pr.read_u32_le()?;
        let mask = pr.read_u8()?;
        let read_write_mask = pr.read_u8()?;
        let stream = pr.read_u8()?;
        let min_precision = pr.read_u8()?;

        let semantic_name = r.read_cstring_at(semantic_name_offset as usize)?;

        parameters.push(DxbcSignatureParameter {
            semantic_name,
            semantic_index,
            system_value_type,
            component_type,
            register,
            mask,
            read_write_mask,
            stream,
            min_precision,
        });
    }

    Ok(DxbcSignature { parameters })
}

#[derive(Clone, Copy)]
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    fourcc: FourCC,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], fourcc: FourCC) -> Self {
        Self {
            bytes,
            pos: 0,
            fourcc,
        }
    }

    fn fork(&self, pos: usize) -> Result<Self, SignatureError> {
        if pos > self.bytes.len() {
            return Err(SignatureError::OutOfBounds {
                fourcc: self.fourcc,
                reason: "fork offset out of bounds",
            });
        }
        Ok(Self {
            bytes: self.bytes,
            pos,
            fourcc: self.fourcc,
        })
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], SignatureError> {
        let end = self.pos.checked_add(len).ok_or(SignatureError::OutOfBounds {
            fourcc: self.fourcc,
            reason: "read offset overflows",
        })?;
        let slice = self.bytes.get(self.pos..end).ok_or(SignatureError::OutOfBounds {
            fourcc: self.fourcc,
            reason: "read past end of chunk",
        })?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, SignatureError> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_u32_le(&mut self) -> Result<u32, SignatureError> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_cstring_at(&self, offset: usize) -> Result<String, SignatureError> {
        if offset >= self.bytes.len() {
            return Err(SignatureError::OutOfBounds {
                fourcc: self.fourcc,
                reason: "cstring offset out of bounds",
            });
        }
        let tail = &self.bytes[offset..];
        let nul = tail.iter().position(|&b| b == 0).ok_or(SignatureError::MalformedChunk {
            fourcc: self.fourcc,
            reason: "unterminated cstring",
        })?;
        let s = std::str::from_utf8(&tail[..nul]).map_err(|_| SignatureError::InvalidUtf8 {
            fourcc: self.fourcc,
            reason: "cstring is not valid UTF-8",
        })?;
        Ok(s.to_owned())
    }
}

