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

fn mip_extent(v: u32, level: u32) -> u32 {
    v.checked_shr(level).unwrap_or(0).max(1)
}

/// Returns whether a BC-compressed texture of the given dimensions can be created under wgpu's
/// WebGPU validation rules.
///
/// WebGPU requires the base mip level dimensions to be aligned to the compression block size
/// (4×4). Additionally, some backends conservatively validate that each mip level whose extent is
/// at least one full block is also aligned.
pub fn wgpu_bc_texture_dimensions_compatible(
    width: u32,
    height: u32,
    mip_level_count: u32,
) -> bool {
    if mip_level_count == 0 {
        return false;
    }

    if width == 0 || height == 0 {
        return false;
    }

    // WebGPU validation requires `mip_level_count` to be within the possible chain length for the
    // given dimensions (regardless of format).
    //
    // This also prevents pathological `mip_level_count` values from causing extremely large loops
    // when this helper is called on untrusted input.
    let max_dim = width.max(height);
    let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
    if mip_level_count > max_mip_levels {
        return false;
    }

    // wgpu/WebGPU validation currently requires the base mip dimensions to be block-aligned for BC
    // formats (4x4 blocks), even when the base mip is smaller than a full block (e.g. 2x2).
    //
    // This differs from some native graphics APIs which allow smaller-than-block base mips.
    if !width.is_multiple_of(4) || !height.is_multiple_of(4) {
        return false;
    }

    for level in 0..mip_level_count {
        let w = mip_extent(width, level);
        let h = mip_extent(height, level);
        // Conservative mip validation (see aero-d3d11's helper): require block alignment for any
        // mip level that still contains at least one full block. (Smaller-than-block mips are
        // allowed.)
        if (w >= 4 && !w.is_multiple_of(4)) || (h >= 4 && !h.is_multiple_of(4)) {
            return false;
        }
    }

    true
}

pub fn select_texture_format(
    requested: TextureFormat,
    caps: GpuCapabilities,
    width: u32,
    height: u32,
    mip_level_count: u32,
) -> TextureFormatSelection {
    if !requested.is_bc_compressed() {
        return TextureFormatSelection {
            requested,
            actual: requested.to_wgpu(),
            upload_transform: TextureUploadTransform::Direct,
        };
    }

    if caps.supports_bc_texture_compression
        && wgpu_bc_texture_dimensions_compatible(width, height, mip_level_count)
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc_dimension_compatibility_requires_block_aligned_base_mip() {
        assert!(!wgpu_bc_texture_dimensions_compatible(1, 1, 1));
        assert!(!wgpu_bc_texture_dimensions_compatible(9, 9, 1));
        assert!(!wgpu_bc_texture_dimensions_compatible(2, 2, 1));
        assert!(wgpu_bc_texture_dimensions_compatible(8, 8, 1));
    }

    #[test]
    fn bc_dimension_compatibility_checks_mip_alignment_conservatively() {
        // mip0 is aligned but mip1 becomes 6x6, which fails the >=4 && multiple-of-4 check.
        assert!(!wgpu_bc_texture_dimensions_compatible(12, 12, 2));
        assert!(wgpu_bc_texture_dimensions_compatible(8, 8, 2));
        // 4x4 -> 2x2 is OK because mip1 is smaller than a block.
        assert!(wgpu_bc_texture_dimensions_compatible(4, 4, 2));
    }

    #[test]
    fn bc_dimension_compatibility_rejects_mip_levels_beyond_possible_chain_length() {
        // WebGPU does not allow mip_level_count to exceed the number of distinct mip extents.
        assert!(!wgpu_bc_texture_dimensions_compatible(1, 1, 2));
        assert!(!wgpu_bc_texture_dimensions_compatible(4, 4, 4)); // max is 3 for 4x4
        assert!(wgpu_bc_texture_dimensions_compatible(4, 4, 3));
    }

    #[test]
    fn selects_rgba8_fallback_for_non_block_aligned_bc_textures_even_when_supported() {
        let caps = GpuCapabilities {
            supports_bc_texture_compression: true,
            ..Default::default()
        };

        let selection = select_texture_format(TextureFormat::Bc1RgbaUnorm, caps, 1, 1, 1);
        assert_eq!(selection.actual, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(
            selection.upload_transform,
            TextureUploadTransform::Bc1ToRgba8
        );

        let selection = select_texture_format(TextureFormat::Bc1RgbaUnorm, caps, 9, 9, 1);
        assert_eq!(selection.actual, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(
            selection.upload_transform,
            TextureUploadTransform::Bc1ToRgba8
        );
    }

    #[test]
    fn selects_native_bc_when_supported_and_block_aligned() {
        let caps = GpuCapabilities {
            supports_bc_texture_compression: true,
            ..Default::default()
        };

        let selection = select_texture_format(TextureFormat::Bc1RgbaUnorm, caps, 8, 8, 1);
        assert_eq!(selection.actual, wgpu::TextureFormat::Bc1RgbaUnorm);
        assert_eq!(selection.upload_transform, TextureUploadTransform::Direct);
    }
}
