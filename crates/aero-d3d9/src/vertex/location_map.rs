use crate::vertex::declaration::DeclUsage;
use std::fmt;
use thiserror::Error;

/// Map D3D9 `(usage, usage_index)` pairs to WGSL `@location(n)`.
///
/// The goal is to keep locations:
/// * deterministic across pipelines (to maximize shader cache hits),
/// * within WebGPU's guaranteed `maxVertexAttributes` lower bound (16),
/// * compatible with both vertex declarations and FVF-derived layouts.
pub trait VertexLocationMap: fmt::Debug + Send + Sync {
    fn location_for(&self, usage: DeclUsage, usage_index: u8) -> Result<u32, LocationMapError>;
}

/// Default mapping used for shader-based pipelines.
///
/// This intentionally fits the most common D3D9 semantics into locations `0..16`:
///
/// | D3D usage          | index | WGSL location |
/// |-------------------|-------|--------------|
/// | POSITION          | 0     | 0            |
/// | NORMAL            | 0     | 1            |
/// | TANGENT           | 0     | 2            |
/// | BINORMAL          | 0     | 3            |
/// | BLENDWEIGHT       | 0     | 4            |
/// | BLENDINDICES      | 0     | 5            |
/// | COLOR             | 0     | 6            |
/// | COLOR             | 1     | 7            |
/// | TEXCOORD          | 0..7  | 8..15        |
#[derive(Debug, Default, Clone, Copy)]
pub struct StandardLocationMap;

impl VertexLocationMap for StandardLocationMap {
    fn location_for(&self, usage: DeclUsage, usage_index: u8) -> Result<u32, LocationMapError> {
        match usage {
            DeclUsage::Position => match usage_index {
                0 => Ok(0),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Normal => match usage_index {
                0 => Ok(1),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Tangent => match usage_index {
                0 => Ok(2),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Binormal => match usage_index {
                0 => Ok(3),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendWeight => match usage_index {
                0 => Ok(4),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendIndices => match usage_index {
                0 => Ok(5),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Color => match usage_index {
                0 => Ok(6),
                1 => Ok(7),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::TexCoord => match usage_index {
                0..=7 => Ok(8 + usage_index as u32),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            other => Err(LocationMapError::UnsupportedSemantic {
                usage: other,
                usage_index,
            }),
        }
    }
}

/// Location map intended for fixed-function / FVF-generated shaders.
///
/// Fixed-function uses a narrower set of semantics than programmable shaders and benefits from a
/// layout that keeps common FVF fields packed at low locations:
///
/// | D3D usage          | index | WGSL location |
/// |-------------------|-------|--------------|
/// | POSITION          | 0     | 0            |
/// | NORMAL            | 0     | 1            |
/// | PSIZE             | 0     | 2            |
/// | COLOR             | 0     | 3            |
/// | COLOR             | 1     | 4            |
/// | TEXCOORD          | 0..7  | 5..12        |
/// | BLENDWEIGHT       | 0     | 13           |
/// | BLENDINDICES      | 0     | 14           |
/// | TANGENT           | 0     | 15           |
///
/// `BINORMAL` is intentionally rejected because it would exceed the guaranteed WebGPU minimum of
/// 16 attributes, and fixed-function content rarely uses it.
#[derive(Debug, Default, Clone, Copy)]
pub struct FixedFunctionLocationMap;

impl VertexLocationMap for FixedFunctionLocationMap {
    fn location_for(&self, usage: DeclUsage, usage_index: u8) -> Result<u32, LocationMapError> {
        match usage {
            DeclUsage::Position => match usage_index {
                0 => Ok(0),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Normal => match usage_index {
                0 => Ok(1),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::PSize => match usage_index {
                0 => Ok(2),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Color => match usage_index {
                0 => Ok(3),
                1 => Ok(4),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::TexCoord => match usage_index {
                0..=7 => Ok(5 + usage_index as u32),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendWeight => match usage_index {
                0 => Ok(13),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::BlendIndices => match usage_index {
                0 => Ok(14),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            DeclUsage::Tangent => match usage_index {
                0 => Ok(15),
                _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
            },
            _ => Err(LocationMapError::UnsupportedSemantic { usage, usage_index }),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LocationMapError {
    #[error("unsupported vertex semantic {usage:?}{usage_index} for WGSL location mapping")]
    UnsupportedSemantic { usage: DeclUsage, usage_index: u8 },
}
