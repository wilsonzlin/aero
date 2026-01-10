use core::fmt;

use aero_dxbc::{DxbcError, DxbcFile, FourCC};

pub const FOURCC_SHDR: FourCC = FourCC(*b"SHDR");
pub const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

pub const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
pub const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
pub const FOURCC_PSGN: FourCC = FourCC(*b"PSGN");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Pixel,
    Geometry,
    Hull,
    Domain,
    Compute,
    Unknown(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderModel {
    pub major: u8,
    pub minor: u8,
}

#[derive(Debug, Clone)]
pub struct Sm4Program {
    pub stage: ShaderStage,
    pub model: ShaderModel,
    /// Full token stream (DWORDs), including version + length.
    pub tokens: Vec<u32>,
}

impl Sm4Program {
    pub fn parse_from_dxbc_bytes(bytes: &[u8]) -> Result<Self, Sm4Error> {
        let file = DxbcFile::parse(bytes)?;
        Self::parse_from_dxbc(&file)
    }

    pub fn parse_from_dxbc(dxbc: &DxbcFile<'_>) -> Result<Self, Sm4Error> {
        // SM4 uses SHDR, SM5 uses SHEX; accept either (prefer SHEX if present).
        let chunk = dxbc
            .get_chunk(FOURCC_SHEX)
            .or_else(|| dxbc.get_chunk(FOURCC_SHDR))
            .or_else(|| dxbc.find_first_shader_chunk())
            .ok_or(Sm4Error::MissingShaderChunk)?;
        Self::parse_program_tokens(chunk.data)
    }

    pub fn parse_program_tokens(bytes: &[u8]) -> Result<Self, Sm4Error> {
        if bytes.len() % 4 != 0 {
            return Err(Sm4Error::MisalignedTokens { len: bytes.len() });
        }
        let mut tokens = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            tokens.push(u32::from_le_bytes(
                chunk.try_into().expect("chunk_exact guarantees 4 bytes"),
            ));
        }
        if tokens.len() < 2 {
            return Err(Sm4Error::TooShort { dwords: tokens.len() });
        }

        let version = tokens[0];
        let declared_len = tokens[1] as usize;
        if declared_len > tokens.len() {
            return Err(Sm4Error::DeclaredLengthOutOfBounds {
                declared: declared_len,
                available: tokens.len(),
            });
        }

        let (stage, model) = decode_version_token(version);

        Ok(Self {
            stage,
            model,
            tokens,
        })
    }
}

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

#[derive(Debug)]
pub enum Sm4Error {
    Dxbc(DxbcError),
    MissingShaderChunk,
    MisalignedTokens { len: usize },
    TooShort { dwords: usize },
    DeclaredLengthOutOfBounds { declared: usize, available: usize },
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
            Sm4Error::DeclaredLengthOutOfBounds { declared, available } => write!(
                f,
                "shader bytecode declares {declared} dwords but only {available} provided"
            ),
        }
    }
}

impl std::error::Error for Sm4Error {}
