//! SM4/SM5 token stream parsing.
//!
//! DXBC shader bytecode for D3D10+ is stored in `SHDR` (SM4) or `SHEX` (SM5)
//! chunks and consists of a stream of 32-bit "tokens" (DWORDs).
//!
//! This module provides a small, **bounds-checked** parser for extracting and
//! validating the token stream from a DXBC container without panicking.

use core::fmt;

use crate::{DxbcError, DxbcFile, FourCC};

/// DXBC chunk ID for SM4 shader bytecode (`SHDR`).
pub const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");
/// DXBC chunk ID for SM5 shader bytecode (`SHEX`).
pub const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

/// Shader stage encoded in an SM4/SM5 version token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    /// Vertex shader.
    Vertex,
    /// Pixel shader.
    Pixel,
    /// Geometry shader.
    Geometry,
    /// Hull shader.
    Hull,
    /// Domain shader.
    Domain,
    /// Compute shader.
    Compute,
    /// Unknown stage type (raw numeric program type).
    Unknown(u16),
}

/// Shader model version (`major.minor`) decoded from the version token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderModel {
    /// Major shader model version (e.g. `4` or `5`).
    pub major: u8,
    /// Minor shader model version (e.g. `0`).
    pub minor: u8,
}

/// A parsed SM4/SM5 program token stream extracted from a DXBC shader bytecode chunk.
#[derive(Debug, Clone)]
pub struct Sm4Program {
    /// Shader stage decoded from the version token.
    pub stage: ShaderStage,
    /// Shader model version decoded from the version token.
    pub model: ShaderModel,
    /// Program token stream (DWORDs), including the version and length tokens.
    ///
    /// The bytecode header contains a declared length (DWORD count) at token index 1; this
    /// `tokens` vector is truncated to that declared length (any trailing bytes in the DXBC chunk
    /// payload are ignored).
    pub tokens: Vec<u32>,
}

/// A parsed SM5 program token stream.
///
/// SM5 uses the same token encoding as SM4, but is typically stored in the
/// `SHEX` chunk rather than `SHDR`.
pub type Sm5Program = Sm4Program;

impl Sm4Program {
    /// Parses a DXBC container from raw bytes and extracts the first SM4/SM5 shader program found.
    pub fn parse_from_dxbc_bytes(bytes: &[u8]) -> Result<Self, Sm4Error> {
        let file = DxbcFile::parse(bytes)?;
        Self::parse_from_dxbc(&file)
    }

    /// Extracts an SM4/SM5 shader program from an already-parsed DXBC container.
    ///
    /// This searches for a shader bytecode chunk in the following order:
    /// 1. `SHEX`
    /// 2. `SHDR`
    /// 3. First chunk that matches either `SHEX` or `SHDR`
    pub fn parse_from_dxbc(dxbc: &DxbcFile<'_>) -> Result<Self, Sm4Error> {
        // SM4 uses SHDR, SM5 uses SHEX; accept either (prefer SHEX if present).
        let chunk = dxbc
            .get_chunk(FOURCC_SHEX)
            .or_else(|| dxbc.get_chunk(FOURCC_SHDR))
            .or_else(|| dxbc.find_first_shader_chunk())
            .ok_or(Sm4Error::MissingShaderChunk)?;
        Self::parse_program_tokens(chunk.data)
    }

    /// Parses an SM4/SM5 token stream from raw bytes (DXBC chunk payload).
    ///
    /// This performs basic validation:
    /// - The byte length must be a multiple of 4.
    /// - At least 2 DWORDs must be present (version + length).
    /// - The declared length (token 1) must be in-bounds and at least 2.
    pub fn parse_program_tokens(bytes: &[u8]) -> Result<Self, Sm4Error> {
        if !bytes.len().is_multiple_of(4) {
            return Err(Sm4Error::MisalignedTokens { len: bytes.len() });
        }
        let available = bytes.len() / 4;
        if available < 2 {
            return Err(Sm4Error::TooShort { dwords: available });
        }

        // Read the header (version + declared length) first so we can avoid allocating and
        // decoding trailing bytes when the stream is truncated or malformed.
        let version = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let declared_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if declared_len < 2 {
            return Err(Sm4Error::DeclaredLengthTooSmall {
                declared: declared_len,
            });
        }
        if declared_len > available {
            return Err(Sm4Error::DeclaredLengthOutOfBounds {
                declared: declared_len,
                available,
            });
        }

        let (stage, model) = decode_version_token(version);

        // Convert the declared token range to u32 tokens (little-endian).
        //
        // Use `try_reserve_exact` so extremely large/corrupt shader blobs don't abort the process
        // via an OOM allocation failure.
        let mut tokens = Vec::new();
        tokens
            .try_reserve_exact(declared_len)
            .map_err(|_| Sm4Error::TokenStreamTooLarge {
                dwords: declared_len,
            })?;
        for chunk in bytes.chunks_exact(4).take(declared_len) {
            tokens.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }

        Ok(Self {
            stage,
            model,
            tokens,
        })
    }
}

/// Decodes an SM4/SM5 version token into `(stage, shader_model)`.
pub fn decode_version_token(version: u32) -> (ShaderStage, ShaderModel) {
    // D3D10+ shader bytecode version token layout:
    // - bits 0..=3: minor version
    // - bits 4..=7: major version
    // - bits 16..=31: program type
    let minor = (version & 0xF) as u8;
    let major = ((version >> 4) & 0xF) as u8;
    let ty = (version >> 16) as u16;

    let stage = match ty {
        0 => ShaderStage::Pixel,
        1 => ShaderStage::Vertex,
        2 => ShaderStage::Geometry,
        3 => ShaderStage::Hull,
        4 => ShaderStage::Domain,
        5 => ShaderStage::Compute,
        other => ShaderStage::Unknown(other),
    };

    (stage, ShaderModel { major, minor })
}

/// Errors that can occur when parsing an SM4/SM5 token stream.
#[derive(Debug)]
pub enum Sm4Error {
    /// The DXBC container failed to parse.
    Dxbc(DxbcError),
    /// The DXBC container does not contain a shader bytecode chunk (`SHDR`/`SHEX`).
    MissingShaderChunk,
    /// The shader bytecode chunk length is not a multiple of 4 bytes.
    MisalignedTokens {
        /// Input byte length.
        len: usize,
    },
    /// The token stream contains fewer than 2 DWORDs.
    TooShort {
        /// Number of DWORDs provided.
        dwords: usize,
    },
    /// The declared program length (DWORD count) is less than 2.
    DeclaredLengthTooSmall {
        /// Declared length in DWORDs.
        declared: usize,
    },
    /// The declared program length (DWORD count) is larger than the provided token stream.
    DeclaredLengthOutOfBounds {
        /// Declared length in DWORDs.
        declared: usize,
        /// Available DWORDs in the token stream.
        available: usize,
    },
    /// The token stream is too large to allocate.
    ///
    /// This can occur either due to an internal capacity overflow or due to the
    /// allocator refusing the request (e.g. OOM).
    TokenStreamTooLarge {
        /// Declared length in DWORDs.
        dwords: usize,
    },
}

impl From<DxbcError> for Sm4Error {
    fn from(value: DxbcError) -> Self {
        Self::Dxbc(value)
    }
}

impl fmt::Display for Sm4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sm4Error::Dxbc(err) => write!(f, "{err}"),
            Sm4Error::MissingShaderChunk => write!(f, "DXBC is missing SHDR/SHEX shader chunk"),
            Sm4Error::MisalignedTokens { len } => {
                write!(f, "shader bytecode length {len} is not a multiple of 4")
            }
            Sm4Error::TooShort { dwords } => {
                write!(f, "shader bytecode too short ({dwords} dwords)")
            }
            Sm4Error::DeclaredLengthTooSmall { declared } => {
                write!(
                    f,
                    "shader bytecode declares invalid length {declared} (< 2)"
                )
            }
            Sm4Error::DeclaredLengthOutOfBounds {
                declared,
                available,
            } => write!(
                f,
                "shader bytecode declares {declared} dwords but only {available} provided"
            ),
            Sm4Error::TokenStreamTooLarge { dwords } => {
                write!(
                    f,
                    "shader bytecode token stream {dwords} dwords is too large to allocate"
                )
            }
        }
    }
}

impl std::error::Error for Sm4Error {}
