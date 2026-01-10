use crate::vertex::declaration::{DeclType, VertexInputError};
use half::f16;
use wgpu::VertexFormat;

/// WebGPU capabilities required for vertex attribute translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WebGpuVertexCaps {
    /// Whether the backend supports 16-bit vertex formats (`float16x2`, `float16x4`, ...).
    pub vertex_attribute_16bit: bool,
}

/// Describes how a D3D vertex element is represented in WebGPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementFormat {
    pub format: VertexFormat,
    pub byte_size: u32,
    pub conversion: ElementConversion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementConversion {
    None,
    /// Convert a half-float vector to 32-bit floats.
    HalfToF32 { components: u8 },
    /// Convert D3D9 `DEC3N` packed 10-10-10 to `float32x3`.
    Dec3NToF32x3,
    /// Convert D3D9 `UDEC3` packed 10-10-10 to `float32x3`.
    UDec3ToF32x3,
}

pub(super) fn map_element_format(ty: DeclType, caps: WebGpuVertexCaps) -> Result<ElementFormat, VertexInputError> {
    let out = match ty {
        DeclType::Float1 => ElementFormat {
            format: VertexFormat::Float32,
            byte_size: 4,
            conversion: ElementConversion::None,
        },
        DeclType::Float2 => ElementFormat {
            format: VertexFormat::Float32x2,
            byte_size: 8,
            conversion: ElementConversion::None,
        },
        DeclType::Float3 => ElementFormat {
            format: VertexFormat::Float32x3,
            byte_size: 12,
            conversion: ElementConversion::None,
        },
        DeclType::Float4 => ElementFormat {
            format: VertexFormat::Float32x4,
            byte_size: 16,
            conversion: ElementConversion::None,
        },
        DeclType::D3dColor | DeclType::UByte4N => ElementFormat {
            format: VertexFormat::Unorm8x4,
            byte_size: 4,
            conversion: ElementConversion::None,
        },
        DeclType::UByte4 => ElementFormat {
            format: VertexFormat::Uint8x4,
            byte_size: 4,
            conversion: ElementConversion::None,
        },
        DeclType::Short2 => ElementFormat {
            format: VertexFormat::Sint16x2,
            byte_size: 4,
            conversion: ElementConversion::None,
        },
        DeclType::Short4 => ElementFormat {
            format: VertexFormat::Sint16x4,
            byte_size: 8,
            conversion: ElementConversion::None,
        },
        DeclType::Short2N => ElementFormat {
            format: VertexFormat::Snorm16x2,
            byte_size: 4,
            conversion: ElementConversion::None,
        },
        DeclType::Short4N => ElementFormat {
            format: VertexFormat::Snorm16x4,
            byte_size: 8,
            conversion: ElementConversion::None,
        },
        DeclType::UShort2N => ElementFormat {
            format: VertexFormat::Unorm16x2,
            byte_size: 4,
            conversion: ElementConversion::None,
        },
        DeclType::UShort4N => ElementFormat {
            format: VertexFormat::Unorm16x4,
            byte_size: 8,
            conversion: ElementConversion::None,
        },
        DeclType::Float16_2 => {
            if caps.vertex_attribute_16bit {
                ElementFormat {
                    format: VertexFormat::Float16x2,
                    byte_size: 4,
                    conversion: ElementConversion::None,
                }
            } else {
                ElementFormat {
                    format: VertexFormat::Float32x2,
                    byte_size: 8,
                    conversion: ElementConversion::HalfToF32 { components: 2 },
                }
            }
        }
        DeclType::Float16_4 => {
            if caps.vertex_attribute_16bit {
                ElementFormat {
                    format: VertexFormat::Float16x4,
                    byte_size: 8,
                    conversion: ElementConversion::None,
                }
            } else {
                ElementFormat {
                    format: VertexFormat::Float32x4,
                    byte_size: 16,
                    conversion: ElementConversion::HalfToF32 { components: 4 },
                }
            }
        }
        DeclType::Dec3N => ElementFormat {
            format: VertexFormat::Float32x3,
            byte_size: 12,
            conversion: ElementConversion::Dec3NToF32x3,
        },
        DeclType::UDec3 => ElementFormat {
            format: VertexFormat::Float32x3,
            byte_size: 12,
            conversion: ElementConversion::UDec3ToF32x3,
        },
        DeclType::Unused => {
            return Err(VertexInputError::UnsupportedDeclType { ty });
        }
    };

    Ok(out)
}

pub(super) fn convert_element(
    plan: &super::ElementConversionPlan,
    src: &[u8],
    dst: &mut [u8],
) -> Result<(), VertexInputError> {
    match plan.conversion {
        ElementConversion::None => {
            let bytes = plan.src_type.byte_size() as usize;
            dst[..bytes].copy_from_slice(&src[..bytes]);
            Ok(())
        }
        ElementConversion::HalfToF32 { components } => {
            let src_bytes = plan.src_type.byte_size() as usize;
            let expected_components = match plan.src_type {
                DeclType::Float16_2 => 2,
                DeclType::Float16_4 => 4,
                _ => components as usize,
            };
            let dst_components = components as usize;
            debug_assert_eq!(expected_components, dst_components);
            debug_assert!(src_bytes >= dst_components * 2);

            for i in 0..dst_components {
                let half_bits = u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]);
                let v = f16::from_bits(half_bits).to_f32();
                dst[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            Ok(())
        }
        ElementConversion::Dec3NToF32x3 => {
            let packed = u32::from_le_bytes(src[..4].try_into().unwrap());
            let (x, y, z) = unpack_dec3n(packed);
            dst[0..4].copy_from_slice(&x.to_le_bytes());
            dst[4..8].copy_from_slice(&y.to_le_bytes());
            dst[8..12].copy_from_slice(&z.to_le_bytes());
            Ok(())
        }
        ElementConversion::UDec3ToF32x3 => {
            let packed = u32::from_le_bytes(src[..4].try_into().unwrap());
            let (x, y, z) = unpack_udec3(packed);
            dst[0..4].copy_from_slice(&(x as f32).to_le_bytes());
            dst[4..8].copy_from_slice(&(y as f32).to_le_bytes());
            dst[8..12].copy_from_slice(&(z as f32).to_le_bytes());
            Ok(())
        }
    }
}

fn unpack_dec3n(packed: u32) -> (f32, f32, f32) {
    // Each component is a signed 10-bit integer.
    let x = sign_extend_10((packed & 0x3ff) as i32);
    let y = sign_extend_10(((packed >> 10) & 0x3ff) as i32);
    let z = sign_extend_10(((packed >> 20) & 0x3ff) as i32);

    (snorm10_to_f32(x), snorm10_to_f32(y), snorm10_to_f32(z))
}

fn unpack_udec3(packed: u32) -> (u32, u32, u32) {
    let x = packed & 0x3ff;
    let y = (packed >> 10) & 0x3ff;
    let z = (packed >> 20) & 0x3ff;
    (x, y, z)
}

fn sign_extend_10(v: i32) -> i32 {
    // v is 10 bits.
    let shift = 32 - 10;
    (v << shift) >> shift
}

fn snorm10_to_f32(v: i32) -> f32 {
    // Signed normalized: [-512, 511] maps to [-1, 1].
    if v == -512 {
        -1.0
    } else {
        (v as f32) / 511.0
    }
}
