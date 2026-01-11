//! Parsing and representation of DXBC signature chunks (`ISGN`/`OSGN`/`PSGN` and
//! `ISG1`/`OSG1`/`PSG1`).
//!
//! The signature chunks provide the semantic ↔ register mapping used by D3D10+
//! shaders. The WGSL translator uses them to generate vertex input/output
//! structs and to provide reflection for input layout construction.

use core::fmt;

use aero_dxbc::signature::parse_signature_chunk_for_fourcc as parse_dxbc_signature_chunk;

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
    MalformedChunk { fourcc: FourCC, reason: String },
    OutOfBounds { fourcc: FourCC, reason: String },
    InvalidUtf8 { fourcc: FourCC, reason: String },
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignatureError::MissingChunk(fourcc) => {
                write!(f, "DXBC missing {fourcc} signature chunk")
            }
            SignatureError::MalformedChunk { fourcc, reason } => {
                write!(f, "malformed DXBC {fourcc} signature chunk: {reason}")
            }
            SignatureError::OutOfBounds { fourcc, reason } => {
                write!(f, "DXBC {fourcc} signature chunk out of bounds: {reason}")
            }
            SignatureError::InvalidUtf8 { fourcc, reason } => {
                write!(
                    f,
                    "DXBC {fourcc} signature chunk contains invalid UTF-8: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for SignatureError {}

pub fn parse_signatures(dxbc: &DxbcFile<'_>) -> Result<ShaderSignatures, SignatureError> {
    const ISG1: FourCC = FourCC(*b"ISG1");
    const OSG1: FourCC = FourCC(*b"OSG1");
    const PSG1: FourCC = FourCC(*b"PSG1");

    Ok(ShaderSignatures {
        // Prefer the `*SG1` variants but accept `*SGN` when needed. Real-world
        // DXBC can contain duplicate signature chunks; we iterate in file order
        // and take the first one that parses successfully.
        isgn: parse_signature_from_dxbc(dxbc, ISG1)?,
        osgn: parse_signature_from_dxbc(dxbc, OSG1)?,
        psgn: parse_signature_from_dxbc(dxbc, PSG1)?,
    })
}

fn parse_signature_from_dxbc(
    dxbc: &DxbcFile<'_>,
    preferred: FourCC,
) -> Result<Option<DxbcSignature>, SignatureError> {
    let fallback = match preferred.0 {
        [b'I', b'S', b'G', b'1'] => Some(FourCC(*b"ISGN")),
        [b'O', b'S', b'G', b'1'] => Some(FourCC(*b"OSGN")),
        [b'P', b'S', b'G', b'1'] => Some(FourCC(*b"PSGN")),
        _ => None,
    };

    let mut first_err = None;
    for chunk in dxbc.get_chunks(preferred) {
        match parse_signature_chunk(chunk.fourcc, chunk.data) {
            Ok(sig) => return Ok(Some(sig)),
            Err(err) => {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }

    if let Some(fallback) = fallback.filter(|&f| f != preferred) {
        for chunk in dxbc.get_chunks(fallback) {
            match parse_signature_chunk(chunk.fourcc, chunk.data) {
                Ok(sig) => return Ok(Some(sig)),
                Err(err) => {
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
            }
        }
    }

    first_err.map_or(Ok(None), Err)
}

pub fn parse_signature_chunk(
    fourcc: FourCC,
    bytes: &[u8],
) -> Result<DxbcSignature, SignatureError> {
    let chunk = parse_dxbc_signature_chunk(fourcc, bytes)
        .map_err(|err| map_dxbc_signature_error(fourcc, err))?;
    convert_dxbc_signature_chunk(fourcc, chunk)
}

fn convert_dxbc_signature_chunk(
    fourcc: FourCC,
    chunk: aero_dxbc::SignatureChunk,
) -> Result<DxbcSignature, SignatureError> {
    let mut parameters = Vec::with_capacity(chunk.entries.len());
    for entry in chunk.entries {
        let stream_u32 = entry.stream.unwrap_or(0);
        // D3D10+ geometry shaders support at most 4 output streams (0..=3). Treat any larger value
        // as malformed input.
        if stream_u32 > 3 {
            return Err(SignatureError::MalformedChunk {
                fourcc,
                reason: "stream index out of range".to_owned(),
            });
        }
        let stream = stream_u32 as u8;

        parameters.push(DxbcSignatureParameter {
            semantic_name: entry.semantic_name,
            semantic_index: entry.semantic_index,
            system_value_type: entry.system_value_type,
            component_type: entry.component_type,
            register: entry.register,
            mask: entry.mask,
            read_write_mask: entry.read_write_mask,
            stream,
            min_precision: 0,
        });
    }

    Ok(DxbcSignature { parameters })
}

fn map_dxbc_signature_error(fourcc: FourCC, err: aero_dxbc::DxbcError) -> SignatureError {
    let reason = err.context().to_owned();
    let is_utf8 = reason.to_ascii_lowercase().contains("utf-8");
    match err {
        aero_dxbc::DxbcError::OutOfBounds { .. } => SignatureError::OutOfBounds { fourcc, reason },
        aero_dxbc::DxbcError::InvalidChunk { .. } if is_utf8 => {
            SignatureError::InvalidUtf8 { fourcc, reason }
        }
        _ => SignatureError::MalformedChunk { fourcc, reason },
    }
}
