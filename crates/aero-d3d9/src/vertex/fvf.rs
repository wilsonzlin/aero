use crate::vertex::declaration::{DeclMethod, DeclType, DeclUsage, VertexDeclaration, VertexElement};
use thiserror::Error;

/// D3D9 Flexible Vertex Format (FVF) code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fvf(pub u32);

/// Decoded FVF layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FvfLayout {
    pub declaration: VertexDeclaration,
    /// Total vertex stride in bytes.
    pub stride: u32,
    /// Whether the position is pre-transformed (XYZRHW).
    pub pretransformed: bool,
}

impl Fvf {
    pub fn decode(self) -> Result<FvfLayout, FvfDecodeError> {
        let fvf = self.0;

        // Position type bits (mask).
        let pos_mask = fvf & 0x0e;
        let (position_ty, pretransformed) = match pos_mask {
            D3DFVF_XYZ => (DeclType::Float3, false),
            D3DFVF_XYZRHW => (DeclType::Float4, true),
            // Blend weights (XYZB1..XYZB5) are more complex; handle the most common case
            // where weights are appended as floats after position.
            D3DFVF_XYZB1 => (DeclType::Float3, false),
            D3DFVF_XYZB2 => (DeclType::Float3, false),
            D3DFVF_XYZB3 => (DeclType::Float3, false),
            D3DFVF_XYZB4 => (DeclType::Float3, false),
            D3DFVF_XYZB5 => (DeclType::Float3, false),
            _ => return Err(FvfDecodeError::MissingPosition { fvf }),
        };

        let mut elements = Vec::new();
        let mut offset = 0u32;

        // Position.
        elements.push(VertexElement::new(
            0,
            offset as u16,
            position_ty,
            DeclMethod::Default,
            DeclUsage::Position,
            0,
        ));
        offset += position_ty.byte_size();

        // Blend weights (FVF encoded in the position mask).
        let blend_weights = match pos_mask {
            D3DFVF_XYZB1 => 1,
            D3DFVF_XYZB2 => 2,
            D3DFVF_XYZB3 => 3,
            D3DFVF_XYZB4 => 4,
            D3DFVF_XYZB5 => 5,
            _ => 0,
        };
        if blend_weights > 0 {
            // D3D9 packs weights as floats. The last weight may be implicit, but we keep the
            // layout explicit for translation purposes.
            elements.push(VertexElement::new(
                0,
                offset as u16,
                match blend_weights {
                    1 => DeclType::Float1,
                    2 => DeclType::Float2,
                    3 => DeclType::Float3,
                    _ => DeclType::Float4,
                },
                DeclMethod::Default,
                DeclUsage::BlendWeight,
                0,
            ));
            offset += match blend_weights {
                1 => 4,
                2 => 8,
                3 => 12,
                4 | 5 => 16,
                _ => 0,
            };

            // Optional blend indices are encoded via LASTBETA flags.
            if (fvf & D3DFVF_LASTBETA_UBYTE4) != 0 {
                elements.push(VertexElement::new(
                    0,
                    offset as u16,
                    DeclType::UByte4,
                    DeclMethod::Default,
                    DeclUsage::BlendIndices,
                    0,
                ));
                offset += 4;
            } else if (fvf & D3DFVF_LASTBETA_D3DCOLOR) != 0 {
                elements.push(VertexElement::new(
                    0,
                    offset as u16,
                    DeclType::D3dColor,
                    DeclMethod::Default,
                    DeclUsage::BlendIndices,
                    0,
                ));
                offset += 4;
            }
        }

        // Normal.
        if (fvf & D3DFVF_NORMAL) != 0 {
            elements.push(VertexElement::new(
                0,
                offset as u16,
                DeclType::Float3,
                DeclMethod::Default,
                DeclUsage::Normal,
                0,
            ));
            offset += 12;
        }

        // Point size.
        if (fvf & D3DFVF_PSIZE) != 0 {
            elements.push(VertexElement::new(
                0,
                offset as u16,
                DeclType::Float1,
                DeclMethod::Default,
                DeclUsage::PSize,
                0,
            ));
            offset += 4;
        }

        // Diffuse and specular colors.
        if (fvf & D3DFVF_DIFFUSE) != 0 {
            elements.push(VertexElement::new(
                0,
                offset as u16,
                DeclType::D3dColor,
                DeclMethod::Default,
                DeclUsage::Color,
                0,
            ));
            offset += 4;
        }
        if (fvf & D3DFVF_SPECULAR) != 0 {
            elements.push(VertexElement::new(
                0,
                offset as u16,
                DeclType::D3dColor,
                DeclMethod::Default,
                DeclUsage::Color,
                1,
            ));
            offset += 4;
        }

        // Texture coordinates.
        let tex_count = ((fvf & D3DFVF_TEXCOUNT_MASK) >> D3DFVF_TEXCOUNT_SHIFT) as u8;
        if tex_count > 8 {
            return Err(FvfDecodeError::TooManyTexCoords { count: tex_count });
        }
        for i in 0..tex_count {
            let dim = texcoord_dim(fvf, i)?;
            let ty = match dim {
                1 => DeclType::Float1,
                2 => DeclType::Float2,
                3 => DeclType::Float3,
                4 => DeclType::Float4,
                _ => unreachable!(),
            };
            elements.push(VertexElement::new(
                0,
                offset as u16,
                ty,
                DeclMethod::Default,
                DeclUsage::TexCoord,
                i,
            ));
            offset += ty.byte_size();
        }

        Ok(FvfLayout {
            declaration: VertexDeclaration { elements },
            stride: offset,
            pretransformed,
        })
    }
}

fn texcoord_dim(fvf: u32, idx: u8) -> Result<u8, FvfDecodeError> {
    let shift = 16 + (idx as u32) * 2;
    let bits = ((fvf >> shift) & 0x3) as u8;
    // In FVF, the value encodes size-1.
    Ok(match bits {
        0 => 2, // default = 2
        1 => 1,
        2 => 3,
        3 => 4,
        _ => return Err(FvfDecodeError::InvalidTexCoordSize { idx, bits }),
    })
}

// Common FVF bit definitions.
const D3DFVF_XYZ: u32 = 0x002;
const D3DFVF_XYZRHW: u32 = 0x004;
const D3DFVF_XYZB1: u32 = 0x006;
const D3DFVF_XYZB2: u32 = 0x008;
const D3DFVF_XYZB3: u32 = 0x00a;
const D3DFVF_XYZB4: u32 = 0x00c;
const D3DFVF_XYZB5: u32 = 0x00e;

const D3DFVF_NORMAL: u32 = 0x010;
const D3DFVF_PSIZE: u32 = 0x020;
const D3DFVF_DIFFUSE: u32 = 0x040;
const D3DFVF_SPECULAR: u32 = 0x080;
const D3DFVF_TEXCOUNT_MASK: u32 = 0x0f00;
const D3DFVF_TEXCOUNT_SHIFT: u32 = 8;

const D3DFVF_LASTBETA_UBYTE4: u32 = 0x1000;
const D3DFVF_LASTBETA_D3DCOLOR: u32 = 0x8000;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FvfDecodeError {
    #[error("FVF code 0x{fvf:08x} does not specify a supported position type")]
    MissingPosition { fvf: u32 },

    #[error("FVF requests {count} texture coordinates; maximum supported is 8")]
    TooManyTexCoords { count: u8 },

    #[error("FVF TEXCOORDSIZE bits for tex{idx} are invalid ({bits})")]
    InvalidTexCoordSize { idx: u8, bits: u8 },
}

