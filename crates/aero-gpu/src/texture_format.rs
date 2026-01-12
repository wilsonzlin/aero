use crate::GpuCapabilities;

/// Texture formats used by the guest / higher layers.
///
/// This enum intentionally mirrors the subset of formats we need for emulation
/// workloads; additional formats can be added as translation expands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Rgba8Unorm,
    Rgba8UnormSrgb,

    Bc1RgbaUnorm,
    Bc1RgbaUnormSrgb,

    Bc2RgbaUnorm,
    Bc2RgbaUnormSrgb,

    Bc3RgbaUnorm,
    Bc3RgbaUnormSrgb,

    Bc7RgbaUnorm,
    Bc7RgbaUnormSrgb,
}

impl TextureFormat {
    pub fn is_srgb(self) -> bool {
        matches!(
            self,
            Self::Rgba8UnormSrgb
                | Self::Bc1RgbaUnormSrgb
                | Self::Bc2RgbaUnormSrgb
                | Self::Bc3RgbaUnormSrgb
                | Self::Bc7RgbaUnormSrgb
        )
    }

    pub fn is_bc_compressed(self) -> bool {
        matches!(
            self,
            Self::Bc1RgbaUnorm
                | Self::Bc1RgbaUnormSrgb
                | Self::Bc2RgbaUnorm
                | Self::Bc2RgbaUnormSrgb
                | Self::Bc3RgbaUnorm
                | Self::Bc3RgbaUnormSrgb
                | Self::Bc7RgbaUnorm
                | Self::Bc7RgbaUnormSrgb
        )
    }

    pub fn fallback_uncompressed(self) -> Self {
        if self.is_srgb() {
            Self::Rgba8UnormSrgb
        } else {
            Self::Rgba8Unorm
        }
    }

    pub fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            Self::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Bc1RgbaUnorm => wgpu::TextureFormat::Bc1RgbaUnorm,
            Self::Bc1RgbaUnormSrgb => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            Self::Bc2RgbaUnorm => wgpu::TextureFormat::Bc2RgbaUnorm,
            Self::Bc2RgbaUnormSrgb => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
            Self::Bc3RgbaUnorm => wgpu::TextureFormat::Bc3RgbaUnorm,
            Self::Bc3RgbaUnormSrgb => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
            Self::Bc7RgbaUnorm => wgpu::TextureFormat::Bc7RgbaUnorm,
            Self::Bc7RgbaUnormSrgb => wgpu::TextureFormat::Bc7RgbaUnormSrgb,
        }
    }

    pub fn bytes_per_pixel(self) -> Option<u32> {
        match self {
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb => Some(4),
            _ => None,
        }
    }

    pub fn bc_block_bytes(self) -> Option<u32> {
        match self {
            Self::Bc1RgbaUnorm | Self::Bc1RgbaUnormSrgb => Some(8),
            Self::Bc2RgbaUnorm | Self::Bc2RgbaUnormSrgb => Some(16),
            Self::Bc3RgbaUnorm | Self::Bc3RgbaUnormSrgb => Some(16),
            Self::Bc7RgbaUnorm | Self::Bc7RgbaUnormSrgb => Some(16),
            _ => None,
        }
    }

    pub fn block_dimensions(self) -> Option<(u32, u32)> {
        if self.is_bc_compressed() {
            Some((4, 4))
        } else {
            None
        }
    }
}

/// How uploaded bytes need to be transformed before reaching the GPU texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureUploadTransform {
    Direct,
    Bc1ToRgba8,
    Bc2ToRgba8,
    Bc3ToRgba8,
    Bc7ToRgba8,
    B5G6R5ToRgba8,
    B5G5R5A1ToRgba8,
}

impl TextureUploadTransform {
    pub fn uses_cpu_decompression(self) -> bool {
        matches!(
            self,
            Self::Bc1ToRgba8 | Self::Bc2ToRgba8 | Self::Bc3ToRgba8 | Self::Bc7ToRgba8
        )
    }
}

/// Result of capability-aware texture format selection.
#[derive(Debug, Clone, Copy)]
pub struct TextureFormatSelection {
    pub requested: TextureFormat,
    pub actual: wgpu::TextureFormat,
    pub upload_transform: TextureUploadTransform,
}

impl TextureFormatSelection {
    pub fn bytes_per_row_for_copy(self, width: u32) -> Option<u32> {
        let bytes = match self.requested {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => width.checked_mul(4)?,
            TextureFormat::Bc1RgbaUnorm
            | TextureFormat::Bc1RgbaUnormSrgb
            | TextureFormat::Bc2RgbaUnorm
            | TextureFormat::Bc2RgbaUnormSrgb
            | TextureFormat::Bc3RgbaUnorm
            | TextureFormat::Bc3RgbaUnormSrgb
            | TextureFormat::Bc7RgbaUnorm
            | TextureFormat::Bc7RgbaUnormSrgb => {
                let blocks_w = width.div_ceil(4);
                let block_bytes = self.requested.bc_block_bytes()?;
                blocks_w.checked_mul(block_bytes)?
            }
        };

        Some(bytes)
    }
}

pub fn select_texture_format(
    requested: TextureFormat,
    caps: GpuCapabilities,
) -> TextureFormatSelection {
    if !requested.is_bc_compressed() {
        return TextureFormatSelection {
            requested,
            actual: requested.to_wgpu(),
            upload_transform: TextureUploadTransform::Direct,
        };
    }

    if caps.supports_bc_texture_compression {
        return TextureFormatSelection {
            requested,
            actual: requested.to_wgpu(),
            upload_transform: TextureUploadTransform::Direct,
        };
    }

    let upload_transform = match requested {
        TextureFormat::Bc1RgbaUnorm | TextureFormat::Bc1RgbaUnormSrgb => {
            TextureUploadTransform::Bc1ToRgba8
        }
        TextureFormat::Bc2RgbaUnorm | TextureFormat::Bc2RgbaUnormSrgb => {
            TextureUploadTransform::Bc2ToRgba8
        }
        TextureFormat::Bc3RgbaUnorm | TextureFormat::Bc3RgbaUnormSrgb => {
            TextureUploadTransform::Bc3ToRgba8
        }
        TextureFormat::Bc7RgbaUnorm | TextureFormat::Bc7RgbaUnormSrgb => {
            TextureUploadTransform::Bc7ToRgba8
        }
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => TextureUploadTransform::Direct,
    };

    let fallback = requested.fallback_uncompressed();
    TextureFormatSelection {
        requested,
        actual: fallback.to_wgpu(),
        upload_transform,
    }
}
