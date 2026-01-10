use crate::backend::BackendKind;

#[derive(Debug, Clone, Copy, Default)]
pub struct TextureCompressionCaps {
    pub bc: bool,
    pub etc2: bool,
    pub astc: bool,
}

impl TextureCompressionCaps {
    pub(crate) fn from_features(features: wgpu::Features) -> Self {
        Self {
            bc: features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            etc2: features.contains(wgpu::Features::TEXTURE_COMPRESSION_ETC2),
            // wgpu exposes ASTC via the `*_ASTC_HDR` flag; browsers treat this as
            // a single "texture-compression-astc" capability.
            astc: features.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackendCaps {
    pub kind: BackendKind,
    pub texture_compression: TextureCompressionCaps,
    pub max_buffer_size: u64,
    pub max_texture_dimension_2d: u32,
}

impl BackendCaps {
    pub(crate) fn from_wgpu(device: &wgpu::Device, kind: BackendKind) -> Self {
        let limits = device.limits();
        let features = device.features();
        Self {
            kind,
            texture_compression: TextureCompressionCaps::from_features(features),
            max_buffer_size: limits.max_buffer_size,
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
        }
    }
}
