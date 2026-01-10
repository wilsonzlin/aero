use crate::vertex::location_map::LocationMapError;
use std::fmt;
use thiserror::Error;

/// D3D9 `D3DVERTEXELEMENT9` (decoded / normalized).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexElement {
    pub stream: u8,
    pub offset: u16,
    pub ty: DeclType,
    pub method: DeclMethod,
    pub usage: DeclUsage,
    pub usage_index: u8,
}

impl VertexElement {
    pub fn new(
        stream: u8,
        offset: u16,
        ty: DeclType,
        method: DeclMethod,
        usage: DeclUsage,
        usage_index: u8,
    ) -> Self {
        Self {
            stream,
            offset,
            ty,
            method,
            usage,
            usage_index,
        }
    }
}

/// D3D9 `IDirect3DVertexDeclaration9` (list of `D3DVERTEXELEMENT9`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VertexDeclaration {
    pub elements: Vec<VertexElement>,
}

/// Raw D3D9 `D3DVERTEXELEMENT9`.
///
/// This is the layout used by the API and in command buffers. Use
/// [`VertexDeclaration::from_d3d_elements`] to decode it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3dVertexElement9 {
    pub stream: u16,
    pub offset: u16,
    pub ty: u8,
    pub method: u8,
    pub usage: u8,
    pub usage_index: u8,
}

impl D3dVertexElement9 {
    pub const BYTE_SIZE: usize = 8;

    pub const fn end() -> Self {
        Self {
            stream: 0xff,
            offset: 0,
            ty: DeclType::Unused as u8,
            method: 0,
            usage: 0,
            usage_index: 0,
        }
    }

    pub fn is_end(self) -> bool {
        self.stream == 0xff
    }

    pub fn from_le_bytes(bytes: [u8; Self::BYTE_SIZE]) -> Self {
        // D3DVERTEXELEMENT9 is little-endian in D3D9 command buffers.
        let stream = u16::from_le_bytes([bytes[0], bytes[1]]);
        let offset = u16::from_le_bytes([bytes[2], bytes[3]]);
        Self {
            stream,
            offset,
            ty: bytes[4],
            method: bytes[5],
            usage: bytes[6],
            usage_index: bytes[7],
        }
    }
}

impl VertexDeclaration {
    /// Decode a serialized D3D9 `D3DVERTEXELEMENT9[]`.
    ///
    /// This expects a stream of 8-byte `D3DVERTEXELEMENT9` structs in little-endian order,
    /// terminated by the standard end marker (`stream=0xFF, type=UNUSED`).
    pub fn from_d3d_bytes(bytes: &[u8]) -> Result<Self, VertexInputError> {
        if bytes.len() % D3dVertexElement9::BYTE_SIZE != 0 {
            return Err(VertexInputError::VertexDeclBytesNotMultipleOf8 { len: bytes.len() });
        }

        let mut raw = Vec::new();
        let mut found_end = false;
        for chunk in bytes.chunks_exact(D3dVertexElement9::BYTE_SIZE) {
            let elem = D3dVertexElement9::from_le_bytes(chunk.try_into().unwrap());
            raw.push(elem);
            if elem.is_end() {
                found_end = true;
                break;
            }
        }

        if !found_end {
            return Err(VertexInputError::VertexDeclMissingEndMarker);
        }

        Self::from_d3d_elements(&raw)
    }

    pub fn from_d3d_elements(elements: &[D3dVertexElement9]) -> Result<Self, VertexInputError> {
        let mut out = Vec::new();
        for &e in elements {
            if e.is_end() {
                break;
            }
            if e.stream >= 16 {
                return Err(VertexInputError::InvalidStreamIndex { stream: e.stream });
            }

            let ty = DeclType::from_u8(e.ty)?;
            if ty == DeclType::Unused {
                // D3D9 uses this value for the sentinel element; some applications also use it as
                // an explicit padding element. Either way, it's not a vertex attribute.
                continue;
            }

            out.push(VertexElement {
                stream: e.stream as u8,
                offset: e.offset,
                ty,
                method: DeclMethod::from_u8(e.method)?,
                usage: DeclUsage::from_u8(e.usage)?,
                usage_index: e.usage_index,
            });
        }
        Ok(Self { elements: out })
    }
}

/// D3D9 `D3DDECLTYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DeclType {
    Float1 = 0,
    Float2 = 1,
    Float3 = 2,
    Float4 = 3,
    D3dColor = 4,
    UByte4 = 5,
    Short2 = 6,
    Short4 = 7,
    UByte4N = 8,
    Short2N = 9,
    Short4N = 10,
    UShort2N = 11,
    UShort4N = 12,
    UDec3 = 13,
    Dec3N = 14,
    Float16_2 = 15,
    Float16_4 = 16,
    Unused = 17,
}

impl DeclType {
    pub fn from_u8(v: u8) -> Result<Self, VertexInputError> {
        let ty = match v {
            0 => Self::Float1,
            1 => Self::Float2,
            2 => Self::Float3,
            3 => Self::Float4,
            4 => Self::D3dColor,
            5 => Self::UByte4,
            6 => Self::Short2,
            7 => Self::Short4,
            8 => Self::UByte4N,
            9 => Self::Short2N,
            10 => Self::Short4N,
            11 => Self::UShort2N,
            12 => Self::UShort4N,
            13 => Self::UDec3,
            14 => Self::Dec3N,
            15 => Self::Float16_2,
            16 => Self::Float16_4,
            17 => Self::Unused,
            other => return Err(VertexInputError::UnknownDeclType { ty: other }),
        };
        Ok(ty)
    }

    pub fn byte_size(self) -> u32 {
        match self {
            Self::Float1 => 4,
            Self::Float2 => 8,
            Self::Float3 => 12,
            Self::Float4 => 16,
            Self::D3dColor => 4,
            Self::UByte4 => 4,
            Self::Short2 => 4,
            Self::Short4 => 8,
            Self::UByte4N => 4,
            Self::Short2N => 4,
            Self::Short4N => 8,
            Self::UShort2N => 4,
            Self::UShort4N => 8,
            Self::UDec3 => 4,
            Self::Dec3N => 4,
            Self::Float16_2 => 4,
            Self::Float16_4 => 8,
            Self::Unused => 0,
        }
    }
}

impl fmt::Display for DeclType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// D3D9 `D3DDECLMETHOD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DeclMethod {
    Default = 0,
    PartialU = 1,
    PartialV = 2,
    CrossUv = 3,
    Uv = 4,
    Lookup = 5,
    LookupPresampled = 6,
}

impl DeclMethod {
    pub fn from_u8(v: u8) -> Result<Self, VertexInputError> {
        let method = match v {
            0 => Self::Default,
            1 => Self::PartialU,
            2 => Self::PartialV,
            3 => Self::CrossUv,
            4 => Self::Uv,
            5 => Self::Lookup,
            6 => Self::LookupPresampled,
            other => return Err(VertexInputError::UnknownDeclMethod { method: other }),
        };
        Ok(method)
    }
}

/// D3D9 `D3DDECLUSAGE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DeclUsage {
    Position = 0,
    BlendWeight = 1,
    BlendIndices = 2,
    Normal = 3,
    PSize = 4,
    TexCoord = 5,
    Tangent = 6,
    Binormal = 7,
    TessFactor = 8,
    PositionT = 9,
    Color = 10,
    Fog = 11,
    Depth = 12,
    Sample = 13,
}

impl DeclUsage {
    pub fn from_u8(v: u8) -> Result<Self, VertexInputError> {
        let usage = match v {
            0 => Self::Position,
            1 => Self::BlendWeight,
            2 => Self::BlendIndices,
            3 => Self::Normal,
            4 => Self::PSize,
            5 => Self::TexCoord,
            6 => Self::Tangent,
            7 => Self::Binormal,
            8 => Self::TessFactor,
            9 => Self::PositionT,
            10 => Self::Color,
            11 => Self::Fog,
            12 => Self::Depth,
            13 => Self::Sample,
            other => return Err(VertexInputError::UnknownDeclUsage { usage: other }),
        };
        Ok(usage)
    }
}

/// Vertex input translation failures.
#[derive(Debug, Error)]
pub enum VertexInputError {
    #[error("missing vertex buffer stride for D3D stream {stream}")]
    MissingStreamStride { stream: u8 },

    #[error("vertex buffer stride for D3D stream {stream} is zero")]
    ZeroStreamStride { stream: u8 },

    #[error("vertex buffer stride for D3D stream {stream} ({stride} bytes) is smaller than required ({required} bytes)")]
    StrideTooSmall { stream: u8, stride: u32, required: u32 },

    #[error("unknown D3DDECLTYPE value {ty}")]
    UnknownDeclType { ty: u8 },

    #[error("unknown D3DDECLMETHOD value {method}")]
    UnknownDeclMethod { method: u8 },

    #[error("unknown D3DDECLUSAGE value {usage}")]
    UnknownDeclUsage { usage: u8 },

    #[error("vertex element stream index {stream} is out of range (expected 0..=15)")]
    InvalidStreamIndex { stream: u16 },

    #[error("vertex declaration byte length ({len}) is not a multiple of 8")]
    VertexDeclBytesNotMultipleOf8 { len: usize },

    #[error("vertex declaration is missing the required end marker (stream=0xFF)")]
    VertexDeclMissingEndMarker,

    #[error(transparent)]
    Location(#[from] LocationMapError),

    #[error("vertex conversion expected {expected} bytes of source data but got {actual} bytes")]
    VertexDataTooSmall { expected: usize, actual: usize },

    #[error("vertex element type {ty} is not supported")]
    UnsupportedDeclType { ty: DeclType },

    #[error("stream source frequency state is invalid: {0}")]
    StreamSourceFreq(#[from] crate::vertex::instancing::StreamSourceFreqParseError),
}
