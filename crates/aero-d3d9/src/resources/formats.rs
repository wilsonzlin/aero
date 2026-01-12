use anyhow::{anyhow, Result};

/// Subset of `D3DFORMAT` needed by the resource layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum D3DFormat {
    /// `D3DFMT_A8R8G8B8`
    A8R8G8B8,
    /// `D3DFMT_X8R8G8B8` (alpha treated as 1.0)
    X8R8G8B8,

    /// `D3DFMT_DXT1` / BC1
    Dxt1,
    /// `D3DFMT_DXT3` / BC2
    Dxt3,
    /// `D3DFMT_DXT5` / BC3
    Dxt5,

    /// `D3DFMT_D16`
    D16,
    /// `D3DFMT_D24S8`
    D24S8,
    /// `D3DFMT_D32`
    D32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureUsageKind {
    Sampled,
    RenderTarget,
    DepthStencil,
}

#[derive(Clone, Copy, Debug)]
pub struct FormatInfo {
    pub d3d: D3DFormat,
    /// D3D memory layout information (what `LockRect` exposes).
    pub d3d_bytes_per_block: u32,
    pub d3d_block_width: u32,
    pub d3d_block_height: u32,
    pub d3d_is_compressed: bool,

    /// GPU upload layout information (what `copy_buffer_to_texture` expects).
    pub upload_bytes_per_block: u32,
    pub upload_block_width: u32,
    pub upload_block_height: u32,
    pub upload_is_compressed: bool,

    pub wgpu: wgpu::TextureFormat,
    /// True when the source data is DXTn but the GPU texture is uncompressed and expects BGRA8.
    pub decompress_to_bgra8: bool,
    /// For formats with unused alpha (X8R8G8B8) we force alpha to 255 on upload.
    pub force_opaque_alpha: bool,
}

impl FormatInfo {
    pub fn mip_dimensions(&self, width: u32, height: u32, level: u32) -> (u32, u32) {
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        (w, h)
    }

    pub fn d3d_mip_level_byte_len(&self, width: u32, height: u32, level: u32) -> usize {
        let (w, h) = self.mip_dimensions(width, height, level);
        let (bw, bh) = if self.d3d_is_compressed {
            (
                w.div_ceil(self.d3d_block_width),
                h.div_ceil(self.d3d_block_height),
            )
        } else {
            (w, h)
        };
        (bw as usize) * (bh as usize) * (self.d3d_bytes_per_block as usize)
    }

    pub fn d3d_mip_level_pitch(&self, width: u32, level: u32) -> u32 {
        let w = (width >> level).max(1);
        let blocks = if self.d3d_is_compressed {
            w.div_ceil(self.d3d_block_width)
        } else {
            w
        };
        blocks * self.d3d_bytes_per_block
    }

    pub fn upload_bytes_per_row(&self, width: u32, level: u32) -> u32 {
        let w = (width >> level).max(1);
        let blocks = if self.upload_is_compressed {
            w.div_ceil(self.upload_block_width)
        } else {
            w
        };
        blocks * self.upload_bytes_per_block
    }

    pub fn upload_rows_per_image(&self, height: u32, level: u32) -> u32 {
        let h = (height >> level).max(1);
        if self.upload_is_compressed {
            h.div_ceil(self.upload_block_height)
        } else {
            h
        }
    }

    pub fn upload_mip_level_byte_len(&self, width: u32, height: u32, level: u32) -> usize {
        let bytes_per_row = self.upload_bytes_per_row(width, level);
        let rows = self.upload_rows_per_image(height, level);
        bytes_per_row as usize * rows as usize
    }
}

pub fn format_info(
    format: D3DFormat,
    device_features: wgpu::Features,
    usage: TextureUsageKind,
) -> Result<FormatInfo> {
    let bc_supported = device_features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC);

    match (format, usage) {
        (D3DFormat::A8R8G8B8, _) => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Bgra8Unorm,
            d3d_bytes_per_block: 4,
            d3d_block_width: 1,
            d3d_block_height: 1,
            d3d_is_compressed: false,
            upload_bytes_per_block: 4,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            decompress_to_bgra8: false,
            force_opaque_alpha: false,
        }),
        (D3DFormat::X8R8G8B8, _) => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Bgra8Unorm,
            d3d_bytes_per_block: 4,
            d3d_block_width: 1,
            d3d_block_height: 1,
            d3d_is_compressed: false,
            upload_bytes_per_block: 4,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            decompress_to_bgra8: false,
            force_opaque_alpha: true,
        }),

        (D3DFormat::Dxt1, _) if bc_supported => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Bc1RgbaUnorm,
            d3d_bytes_per_block: 8,
            d3d_block_width: 4,
            d3d_block_height: 4,
            d3d_is_compressed: true,
            upload_bytes_per_block: 8,
            upload_block_width: 4,
            upload_block_height: 4,
            upload_is_compressed: true,
            decompress_to_bgra8: false,
            force_opaque_alpha: false,
        }),
        (D3DFormat::Dxt3, _) if bc_supported => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Bc2RgbaUnorm,
            d3d_bytes_per_block: 16,
            d3d_block_width: 4,
            d3d_block_height: 4,
            d3d_is_compressed: true,
            upload_bytes_per_block: 16,
            upload_block_width: 4,
            upload_block_height: 4,
            upload_is_compressed: true,
            decompress_to_bgra8: false,
            force_opaque_alpha: false,
        }),
        (D3DFormat::Dxt5, _) if bc_supported => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Bc3RgbaUnorm,
            d3d_bytes_per_block: 16,
            d3d_block_width: 4,
            d3d_block_height: 4,
            d3d_is_compressed: true,
            upload_bytes_per_block: 16,
            upload_block_width: 4,
            upload_block_height: 4,
            upload_is_compressed: true,
            decompress_to_bgra8: false,
            force_opaque_alpha: false,
        }),

        (D3DFormat::Dxt1, _) => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Bgra8Unorm,
            d3d_bytes_per_block: 8,
            d3d_block_width: 4,
            d3d_block_height: 4,
            d3d_is_compressed: true,
            upload_bytes_per_block: 4,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            decompress_to_bgra8: true,
            force_opaque_alpha: false,
        }),
        (D3DFormat::Dxt3 | D3DFormat::Dxt5, _) => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Bgra8Unorm,
            d3d_bytes_per_block: 16,
            d3d_block_width: 4,
            d3d_block_height: 4,
            d3d_is_compressed: true,
            upload_bytes_per_block: 4,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            decompress_to_bgra8: true,
            force_opaque_alpha: false,
        }),

        (D3DFormat::D16, TextureUsageKind::DepthStencil) => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Depth16Unorm,
            d3d_bytes_per_block: 2,
            d3d_block_width: 1,
            d3d_block_height: 1,
            d3d_is_compressed: false,
            upload_bytes_per_block: 2,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            decompress_to_bgra8: false,
            force_opaque_alpha: false,
        }),
        (D3DFormat::D24S8, TextureUsageKind::DepthStencil) => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Depth24PlusStencil8,
            d3d_bytes_per_block: 4,
            d3d_block_width: 1,
            d3d_block_height: 1,
            d3d_is_compressed: false,
            upload_bytes_per_block: 4,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            decompress_to_bgra8: false,
            force_opaque_alpha: false,
        }),
        (D3DFormat::D32, TextureUsageKind::DepthStencil) => Ok(FormatInfo {
            d3d: format,
            wgpu: wgpu::TextureFormat::Depth32Float,
            d3d_bytes_per_block: 4,
            d3d_block_width: 1,
            d3d_block_height: 1,
            d3d_is_compressed: false,
            upload_bytes_per_block: 4,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            decompress_to_bgra8: false,
            force_opaque_alpha: false,
        }),

        (D3DFormat::D16 | D3DFormat::D24S8 | D3DFormat::D32, _) => Err(anyhow!(
            "depth format {:?} must be used with TextureUsageKind::DepthStencil",
            format
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxt_formats_use_bc_when_supported() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;

        for (format, expected_wgpu) in [
            (D3DFormat::Dxt1, wgpu::TextureFormat::Bc1RgbaUnorm),
            (D3DFormat::Dxt3, wgpu::TextureFormat::Bc2RgbaUnorm),
            (D3DFormat::Dxt5, wgpu::TextureFormat::Bc3RgbaUnorm),
        ] {
            let info = format_info(format, features, TextureUsageKind::Sampled).unwrap();
            assert_eq!(info.wgpu, expected_wgpu);
            assert!(!info.decompress_to_bgra8);
            assert!(info.upload_is_compressed);
            assert!(info.d3d_is_compressed);
        }
    }

    #[test]
    fn dxt_formats_fallback_to_bgra8_when_bc_unsupported() {
        let features = wgpu::Features::empty();

        for format in [D3DFormat::Dxt1, D3DFormat::Dxt3, D3DFormat::Dxt5] {
            let info = format_info(format, features, TextureUsageKind::Sampled).unwrap();
            assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
            assert!(info.decompress_to_bgra8);
            assert!(!info.upload_is_compressed);
            assert!(info.d3d_is_compressed);
        }
    }
}

pub fn align_copy_bytes_per_row(bytes_per_row: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    bytes_per_row.div_ceil(align) * align
}
