use anyhow::{anyhow, Result};

/// Subset of `D3DFORMAT` needed by the resource layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum D3DFormat {
    /// `D3DFMT_A8R8G8B8`
    A8R8G8B8,
    /// `D3DFMT_X8R8G8B8` (alpha treated as 1.0)
    X8R8G8B8,

    /// `D3DFMT_R5G6B5` (alpha treated as 1.0)
    R5G6B5,
    /// `D3DFMT_A1R5G5B5`
    A1R5G5B5,
    /// `D3DFMT_X1R5G5B5` (alpha treated as 1.0)
    X1R5G5B5,
    /// `D3DFMT_A4R4G4B4`
    A4R4G4B4,

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
    /// True when the source data is not in the same layout/format as the GPU texture and we must
    /// convert on the CPU before staging the upload (e.g. BCn decompression or 16-bit packed color
    /// expansion to BGRA8).
    pub cpu_convert_to_bgra8: bool,
    /// For formats with unused alpha (X8R8G8B8) we force alpha to 255 on upload.
    pub force_opaque_alpha: bool,
}

impl FormatInfo {
    pub fn mip_dimensions(&self, width: u32, height: u32, level: u32) -> (u32, u32) {
        let w = width.checked_shr(level).unwrap_or(0).max(1);
        let h = height.checked_shr(level).unwrap_or(0).max(1);
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
        let w = width.checked_shr(level).unwrap_or(0).max(1);
        let blocks = if self.d3d_is_compressed {
            w.div_ceil(self.d3d_block_width)
        } else {
            w
        };
        blocks * self.d3d_bytes_per_block
    }

    pub fn upload_bytes_per_row(&self, width: u32, level: u32) -> u32 {
        let w = width.checked_shr(level).unwrap_or(0).max(1);
        let blocks = if self.upload_is_compressed {
            w.div_ceil(self.upload_block_width)
        } else {
            w
        };
        blocks * self.upload_bytes_per_block
    }

    pub fn upload_rows_per_image(&self, height: u32, level: u32) -> u32 {
        let h = height.checked_shr(level).unwrap_or(0).max(1);
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

fn mip_extent(v: u32, level: u32) -> u32 {
    v.checked_shr(level).unwrap_or(0).max(1)
}

/// Returns whether a BC-compressed texture of the given dimensions can be created under wgpu's
/// WebGPU validation rules.
///
/// wgpu/WebGPU validation currently requires the base mip dimensions to be block-aligned for BC
/// formats (4×4 blocks), even when smaller than a full block (e.g. 1×1 or 2×2 BC textures).
///
/// Additionally, some backends conservatively validate that each mip level whose extent is at
/// least one full block is also block-aligned. (Smaller-than-block mips are allowed.)
pub fn wgpu_bc_texture_dimensions_compatible(width: u32, height: u32, mip_levels: u32) -> bool {
    if mip_levels == 0 {
        return false;
    }

    if width == 0 || height == 0 {
        return false;
    }

    // WebGPU validation requires `mip_level_count` to be within the possible chain length for the
    // given dimensions (regardless of format).
    //
    // This also prevents pathological `mip_levels` values from causing extremely large loops when
    // this helper is called on untrusted input.
    let max_dim = width.max(height);
    let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
    if mip_levels > max_mip_levels {
        return false;
    }

    // wgpu/WebGPU validation currently requires the base mip dimensions to be block-aligned (4x4 for
    // BC formats), even when the base mip is smaller than a full block (e.g. 1x1/2x2).
    if !width.is_multiple_of(4) || !height.is_multiple_of(4) {
        return false;
    }

    for level in 0..mip_levels {
        let w = mip_extent(width, level);
        let h = mip_extent(height, level);

        // Conservative mip validation: require block alignment for any mip level that still
        // contains at least one full block. (Smaller-than-block mips are allowed.)
        if (w >= 4 && !w.is_multiple_of(4)) || (h >= 4 && !h.is_multiple_of(4)) {
            return false;
        }
    }

    true
}

pub fn format_info_for_texture(
    format: D3DFormat,
    device_features: wgpu::Features,
    usage: TextureUsageKind,
    width: u32,
    height: u32,
    mip_levels: u32,
) -> Result<FormatInfo> {
    let info = format_info(format, device_features, usage)?;

    // Even when BC formats are supported, wgpu/WebGPU validation requires block-aligned base
    // dimensions and (conservatively on some backends) block-aligned mips. Fall back to the
    // existing BGRA8+CPU-decompression path when dimensions are incompatible.
    if matches!(format, D3DFormat::Dxt1 | D3DFormat::Dxt3 | D3DFormat::Dxt5)
        && matches!(
            info.wgpu,
            wgpu::TextureFormat::Bc1RgbaUnorm
                | wgpu::TextureFormat::Bc2RgbaUnorm
                | wgpu::TextureFormat::Bc3RgbaUnorm
        )
        && !wgpu_bc_texture_dimensions_compatible(width, height, mip_levels)
    {
        let features_without_bc = device_features & !wgpu::Features::TEXTURE_COMPRESSION_BC;
        return format_info(format, features_without_bc, usage);
    }

    Ok(info)
}

pub fn format_info(
    format: D3DFormat,
    device_features: wgpu::Features,
    usage: TextureUsageKind,
) -> Result<FormatInfo> {
    let bc_supported = device_features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    let is_depth = matches!(format, D3DFormat::D16 | D3DFormat::D24S8 | D3DFormat::D32);
    let is_compressed = matches!(format, D3DFormat::Dxt1 | D3DFormat::Dxt3 | D3DFormat::Dxt5);

    // Validate usage combinations up-front to avoid creating textures that will fail WebGPU
    // validation (and to match D3D9 usage rules).
    match usage {
        TextureUsageKind::DepthStencil if !is_depth => {
            return Err(anyhow!(
                "non-depth format {:?} cannot be used with TextureUsageKind::DepthStencil",
                format
            ));
        }
        TextureUsageKind::RenderTarget if is_compressed => {
            return Err(anyhow!(
                "compressed format {:?} must be used with TextureUsageKind::Sampled",
                format
            ));
        }
        _ => {}
    }

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
            cpu_convert_to_bgra8: false,
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
            cpu_convert_to_bgra8: false,
            force_opaque_alpha: true,
        }),

        (
            D3DFormat::R5G6B5 | D3DFormat::A1R5G5B5 | D3DFormat::X1R5G5B5 | D3DFormat::A4R4G4B4,
            _,
        ) => Ok(FormatInfo {
            // These packed 16-bit formats are commonly used by D3D9 games, but WebGPU doesn't
            // require native support on all backends. We store them as BGRA8 on the GPU and expand
            // on upload.
            d3d: format,
            wgpu: wgpu::TextureFormat::Bgra8Unorm,
            d3d_bytes_per_block: 2,
            d3d_block_width: 1,
            d3d_block_height: 1,
            d3d_is_compressed: false,
            upload_bytes_per_block: 4,
            upload_block_width: 1,
            upload_block_height: 1,
            upload_is_compressed: false,
            cpu_convert_to_bgra8: true,
            force_opaque_alpha: false,
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
            cpu_convert_to_bgra8: false,
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
            cpu_convert_to_bgra8: false,
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
            cpu_convert_to_bgra8: false,
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
            cpu_convert_to_bgra8: true,
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
            cpu_convert_to_bgra8: true,
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
            cpu_convert_to_bgra8: false,
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
            cpu_convert_to_bgra8: false,
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
            cpu_convert_to_bgra8: false,
            force_opaque_alpha: false,
        }),

        (D3DFormat::D16 | D3DFormat::D24S8 | D3DFormat::D32, _) => Err(anyhow!(
            "depth format {:?} must be used with TextureUsageKind::DepthStencil",
            format
        )),
    }
}

pub fn align_copy_bytes_per_row(bytes_per_row: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    bytes_per_row.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! d3d_formats {
        ($($variant:ident),+ $(,)?) => {
            // Keep an explicit list for table-driven tests.
            const ALL_FORMATS: &[D3DFormat] = &[$(D3DFormat::$variant),+];

            // Ensure the list above stays in sync with the enum definition: adding a new
            // `D3DFormat` variant will fail to compile until it is included in this list.
            fn assert_d3d_format_is_exhaustive(f: D3DFormat) {
                match f {
                    $(D3DFormat::$variant => {},)+
                }
            }
        };
    }

    d3d_formats![
        A8R8G8B8,
        X8R8G8B8,
        R5G6B5,
        A1R5G5B5,
        X1R5G5B5,
        A4R4G4B4,
        Dxt1,
        Dxt3,
        Dxt5,
        D16,
        D24S8,
        D32,
    ];

    macro_rules! texture_usages {
        ($($variant:ident),+ $(,)?) => {
            const ALL_USAGES: &[TextureUsageKind] = &[$(TextureUsageKind::$variant),+];

            // Ensure new `TextureUsageKind` variants force test updates (no wildcard arm).
            fn assert_texture_usage_kind_is_exhaustive(u: TextureUsageKind) {
                match u {
                    $(TextureUsageKind::$variant => {},)+
                }
            }
        };
    }

    texture_usages![Sampled, RenderTarget, DepthStencil];

    #[test]
    fn format_test_tables_have_no_duplicates() {
        for (i, &a) in ALL_FORMATS.iter().enumerate() {
            for &b in &ALL_FORMATS[i + 1..] {
                assert_ne!(a, b, "ALL_FORMATS contains duplicate entry {a:?}");
            }
        }

        for (i, &a) in ALL_USAGES.iter().enumerate() {
            for &b in &ALL_USAGES[i + 1..] {
                assert_ne!(a, b, "ALL_USAGES contains duplicate entry {a:?}");
            }
        }
    }

    #[test]
    fn format_info_errors_for_invalid_usage_combinations_are_stable() {
        let features = wgpu::Features::empty();

        let err = format_info(
            D3DFormat::A8R8G8B8,
            features,
            TextureUsageKind::DepthStencil,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("non-depth format") && err.contains("DepthStencil"),
            "unexpected error message: {err}"
        );

        let err = format_info(D3DFormat::D16, features, TextureUsageKind::Sampled)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("depth format") && err.contains("DepthStencil"),
            "unexpected error message: {err}"
        );

        let err = format_info(D3DFormat::Dxt1, features, TextureUsageKind::RenderTarget)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("compressed format") && err.contains("Sampled"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn format_info_is_exhaustively_tested_and_validates_usage_pairs() {
        // `ALL_FORMATS`/`ALL_USAGES` are used by the tests below and should be updated whenever new
        // variants are introduced. The `expected_ok` match further below is intentionally
        // exhaustive (no wildcard arm) so adding a new `D3DFormat` variant will fail to compile
        // until the expected behavior is captured in this test.

        #[derive(Debug)]
        struct ExpectedOk {
            wgpu: wgpu::TextureFormat,
            d3d_bytes_per_block: u32,
            d3d_block_width: u32,
            d3d_block_height: u32,
            d3d_is_compressed: bool,
            upload_bytes_per_block: u32,
            upload_block_width: u32,
            upload_block_height: u32,
            upload_is_compressed: bool,
            cpu_convert_to_bgra8: bool,
            force_opaque_alpha: bool,
        }

        fn expected_ok(format: D3DFormat, bc_supported: bool) -> ExpectedOk {
            match format {
                D3DFormat::A8R8G8B8 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bgra8Unorm,
                    d3d_bytes_per_block: 4,
                    d3d_block_width: 1,
                    d3d_block_height: 1,
                    d3d_is_compressed: false,
                    upload_bytes_per_block: 4,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: false,
                },
                D3DFormat::X8R8G8B8 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bgra8Unorm,
                    d3d_bytes_per_block: 4,
                    d3d_block_width: 1,
                    d3d_block_height: 1,
                    d3d_is_compressed: false,
                    upload_bytes_per_block: 4,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: true,
                },

                D3DFormat::R5G6B5
                | D3DFormat::A1R5G5B5
                | D3DFormat::X1R5G5B5
                | D3DFormat::A4R4G4B4 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bgra8Unorm,
                    d3d_bytes_per_block: 2,
                    d3d_block_width: 1,
                    d3d_block_height: 1,
                    d3d_is_compressed: false,
                    upload_bytes_per_block: 4,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: true,
                    force_opaque_alpha: false,
                },

                D3DFormat::Dxt1 if bc_supported => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bc1RgbaUnorm,
                    d3d_bytes_per_block: 8,
                    d3d_block_width: 4,
                    d3d_block_height: 4,
                    d3d_is_compressed: true,
                    upload_bytes_per_block: 8,
                    upload_block_width: 4,
                    upload_block_height: 4,
                    upload_is_compressed: true,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: false,
                },
                D3DFormat::Dxt3 if bc_supported => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bc2RgbaUnorm,
                    d3d_bytes_per_block: 16,
                    d3d_block_width: 4,
                    d3d_block_height: 4,
                    d3d_is_compressed: true,
                    upload_bytes_per_block: 16,
                    upload_block_width: 4,
                    upload_block_height: 4,
                    upload_is_compressed: true,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: false,
                },
                D3DFormat::Dxt5 if bc_supported => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bc3RgbaUnorm,
                    d3d_bytes_per_block: 16,
                    d3d_block_width: 4,
                    d3d_block_height: 4,
                    d3d_is_compressed: true,
                    upload_bytes_per_block: 16,
                    upload_block_width: 4,
                    upload_block_height: 4,
                    upload_is_compressed: true,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: false,
                },

                // DXTn fallback path: always upload as BGRA8 with CPU decompression.
                D3DFormat::Dxt1 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bgra8Unorm,
                    d3d_bytes_per_block: 8,
                    d3d_block_width: 4,
                    d3d_block_height: 4,
                    d3d_is_compressed: true,
                    upload_bytes_per_block: 4,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: true,
                    force_opaque_alpha: false,
                },
                D3DFormat::Dxt3 | D3DFormat::Dxt5 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Bgra8Unorm,
                    d3d_bytes_per_block: 16,
                    d3d_block_width: 4,
                    d3d_block_height: 4,
                    d3d_is_compressed: true,
                    upload_bytes_per_block: 4,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: true,
                    force_opaque_alpha: false,
                },

                D3DFormat::D16 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Depth16Unorm,
                    d3d_bytes_per_block: 2,
                    d3d_block_width: 1,
                    d3d_block_height: 1,
                    d3d_is_compressed: false,
                    upload_bytes_per_block: 2,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: false,
                },
                D3DFormat::D24S8 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Depth24PlusStencil8,
                    d3d_bytes_per_block: 4,
                    d3d_block_width: 1,
                    d3d_block_height: 1,
                    d3d_is_compressed: false,
                    upload_bytes_per_block: 4,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: false,
                },
                D3DFormat::D32 => ExpectedOk {
                    wgpu: wgpu::TextureFormat::Depth32Float,
                    d3d_bytes_per_block: 4,
                    d3d_block_width: 1,
                    d3d_block_height: 1,
                    d3d_is_compressed: false,
                    upload_bytes_per_block: 4,
                    upload_block_width: 1,
                    upload_block_height: 1,
                    upload_is_compressed: false,
                    cpu_convert_to_bgra8: false,
                    force_opaque_alpha: false,
                },
            }
        }

        for bc_supported in [false, true] {
            let features = if bc_supported {
                wgpu::Features::TEXTURE_COMPRESSION_BC
            } else {
                wgpu::Features::empty()
            };

            for &format in ALL_FORMATS {
                for &usage in ALL_USAGES {
                    assert_d3d_format_is_exhaustive(format);
                    assert_texture_usage_kind_is_exhaustive(usage);

                    let res = format_info(format, features, usage);

                    let expects_ok = matches!(
                        (format, usage),
                        (
                            D3DFormat::A8R8G8B8
                                | D3DFormat::X8R8G8B8
                                | D3DFormat::R5G6B5
                                | D3DFormat::A1R5G5B5
                                | D3DFormat::X1R5G5B5
                                | D3DFormat::A4R4G4B4,
                            TextureUsageKind::Sampled | TextureUsageKind::RenderTarget,
                        ) | (
                            D3DFormat::Dxt1 | D3DFormat::Dxt3 | D3DFormat::Dxt5,
                            TextureUsageKind::Sampled,
                        ) | (
                            D3DFormat::D16 | D3DFormat::D24S8 | D3DFormat::D32,
                            TextureUsageKind::DepthStencil,
                        )
                    );

                    assert_eq!(
                        res.is_ok(),
                        expects_ok,
                        "format_info({format:?}, bc_supported={bc_supported}, usage={usage:?})"
                    );

                    if expects_ok {
                        let info = res.unwrap();
                        let expected = expected_ok(format, bc_supported);
                        assert_eq!(info.d3d, format);
                        assert_eq!(info.wgpu, expected.wgpu);
                        assert_eq!(info.d3d_bytes_per_block, expected.d3d_bytes_per_block);
                        assert_eq!(info.d3d_block_width, expected.d3d_block_width);
                        assert_eq!(info.d3d_block_height, expected.d3d_block_height);
                        assert_eq!(info.d3d_is_compressed, expected.d3d_is_compressed);
                        assert_eq!(info.upload_bytes_per_block, expected.upload_bytes_per_block);
                        assert_eq!(info.upload_block_width, expected.upload_block_width);
                        assert_eq!(info.upload_block_height, expected.upload_block_height);
                        assert_eq!(info.upload_is_compressed, expected.upload_is_compressed);
                        assert_eq!(info.cpu_convert_to_bgra8, expected.cpu_convert_to_bgra8);
                        assert_eq!(info.force_opaque_alpha, expected.force_opaque_alpha);
                    } else {
                        assert!(res.is_err());
                    }
                }
            }
        }
    }

    #[test]
    fn bc_dimension_compatibility_requires_block_aligned_base_mip() {
        assert!(!wgpu_bc_texture_dimensions_compatible(0, 4, 1));
        assert!(!wgpu_bc_texture_dimensions_compatible(4, 0, 1));
        assert!(!wgpu_bc_texture_dimensions_compatible(4, 4, 0));
        assert!(!wgpu_bc_texture_dimensions_compatible(1, 1, 1));
        assert!(!wgpu_bc_texture_dimensions_compatible(9, 9, 1));
        assert!(!wgpu_bc_texture_dimensions_compatible(2, 2, 1));
        assert!(wgpu_bc_texture_dimensions_compatible(8, 8, 1));
    }

    #[test]
    fn bc_dimension_compatibility_checks_mip_alignment_conservatively() {
        // mip0 is aligned but mip1 becomes 6x6, which fails the >=4 && multiple-of-4 check.
        assert!(!wgpu_bc_texture_dimensions_compatible(12, 12, 2));
        // 8x8 -> 4x4 is fine.
        assert!(wgpu_bc_texture_dimensions_compatible(8, 8, 2));
        // 4x4 -> 2x2 is OK because mip1 is smaller than a full block.
        assert!(wgpu_bc_texture_dimensions_compatible(4, 4, 2));
    }

    #[test]
    fn bc_dimension_compatibility_rejects_mip_levels_beyond_possible_chain_length() {
        // WebGPU does not allow mip_level_count to exceed the number of distinct mip extents.
        assert!(!wgpu_bc_texture_dimensions_compatible(4, 4, 4)); // max is 3 for 4x4
        assert!(wgpu_bc_texture_dimensions_compatible(4, 4, 3));
    }

    #[test]
    fn bc_dimension_compatibility_rejects_pathological_mip_level_counts() {
        // Should fast-fail extremely large values without looping.
        assert!(!wgpu_bc_texture_dimensions_compatible(4, 4, u32::MAX));
        assert!(!wgpu_bc_texture_dimensions_compatible(1, 1, u32::MAX));
    }

    #[test]
    fn dxt_format_selection_uses_bc_only_when_supported_and_dimensions_compatible() {
        let bc_features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let no_bc = wgpu::Features::empty();

        for (format, expected_bc) in [
            (D3DFormat::Dxt1, wgpu::TextureFormat::Bc1RgbaUnorm),
            (D3DFormat::Dxt3, wgpu::TextureFormat::Bc2RgbaUnorm),
            (D3DFormat::Dxt5, wgpu::TextureFormat::Bc3RgbaUnorm),
        ] {
            // Feature off: always BGRA8 + CPU conversion.
            let info =
                format_info_for_texture(format, no_bc, TextureUsageKind::Sampled, 8, 8, 2).unwrap();
            assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
            assert!(info.cpu_convert_to_bgra8);

            // Feature on but dimensions incompatible: still use BGRA8.
            let info =
                format_info_for_texture(format, bc_features, TextureUsageKind::Sampled, 1, 1, 1)
                    .unwrap();
            assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
            assert!(info.cpu_convert_to_bgra8);

            // Feature on but mips incompatible: still use BGRA8.
            let info =
                format_info_for_texture(format, bc_features, TextureUsageKind::Sampled, 12, 12, 2)
                    .unwrap();
            assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
            assert!(info.cpu_convert_to_bgra8);

            // Feature on and dimensions compatible: use native BC texture.
            let info =
                format_info_for_texture(format, bc_features, TextureUsageKind::Sampled, 8, 8, 2)
                    .unwrap();
            assert_eq!(info.wgpu, expected_bc);
            assert!(!info.cpu_convert_to_bgra8);
            assert!(info.upload_is_compressed);
        }
    }

    #[test]
    fn bc_format_selection_matches_bc_dimension_compatibility_helper() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;

        for (format, expected_bc) in [
            (D3DFormat::Dxt1, wgpu::TextureFormat::Bc1RgbaUnorm),
            (D3DFormat::Dxt3, wgpu::TextureFormat::Bc2RgbaUnorm),
            (D3DFormat::Dxt5, wgpu::TextureFormat::Bc3RgbaUnorm),
        ] {
            let expected_d3d_bytes_per_block = match format {
                D3DFormat::Dxt1 => 8,
                D3DFormat::Dxt3 | D3DFormat::Dxt5 => 16,
                _ => unreachable!(),
            };

            // Cover a range of small dimensions and mip counts, including edge cases like
            // non-block-aligned sizes and mip counts beyond the possible chain length.
            for width in 1..=16 {
                for height in 1..=16 {
                    for mip_levels in 1..=5 {
                        let compatible =
                            wgpu_bc_texture_dimensions_compatible(width, height, mip_levels);
                        let info = format_info_for_texture(
                            format,
                            features,
                            TextureUsageKind::Sampled,
                            width,
                            height,
                            mip_levels,
                        )
                        .unwrap();

                        // DXT formats are always compressed in the D3D memory layout, regardless of
                        // whether we fall back to uncompressed BGRA8 on the GPU.
                        assert!(info.d3d_is_compressed);
                        assert_eq!(info.d3d_bytes_per_block, expected_d3d_bytes_per_block);
                        assert_eq!(info.d3d_block_width, 4);
                        assert_eq!(info.d3d_block_height, 4);

                        if compatible {
                            assert_eq!(info.wgpu, expected_bc);
                            assert!(!info.cpu_convert_to_bgra8);
                            assert!(info.upload_is_compressed);
                            assert_eq!(info.upload_bytes_per_block, expected_d3d_bytes_per_block);
                            assert_eq!(info.upload_block_width, 4);
                            assert_eq!(info.upload_block_height, 4);
                        } else {
                            assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
                            assert!(info.cpu_convert_to_bgra8);
                            assert!(!info.upload_is_compressed);
                            assert_eq!(info.upload_bytes_per_block, 4);
                            assert_eq!(info.upload_block_width, 1);
                            assert_eq!(info.upload_block_height, 1);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mip_helpers_match_expected_memory_layouts() {
        // Uncompressed BGRA8.
        let bgra8 = format_info(
            D3DFormat::A8R8G8B8,
            wgpu::Features::empty(),
            TextureUsageKind::Sampled,
        )
        .unwrap();
        assert_eq!(bgra8.d3d_mip_level_pitch(7, 0), 7 * 4);
        assert_eq!(bgra8.d3d_mip_level_byte_len(7, 5, 0), 7 * 5 * 4);
        assert_eq!(bgra8.d3d_mip_level_pitch(7, 1), 3 * 4);
        assert_eq!(bgra8.d3d_mip_level_byte_len(7, 5, 1), 3 * 2 * 4);
        assert_eq!(bgra8.d3d_mip_level_pitch(7, 2), 4);
        assert_eq!(bgra8.d3d_mip_level_byte_len(7, 5, 2), 4);
        assert_eq!(bgra8.upload_bytes_per_row(7, 0), 7 * 4);
        assert_eq!(bgra8.upload_rows_per_image(5, 0), 5);
        assert_eq!(bgra8.upload_mip_level_byte_len(7, 5, 0), 7 * 5 * 4);

        // Packed 16-bit layout (D3D pitch uses 2 bytes/px, GPU upload uses BGRA8).
        let r5g6b5 = format_info(
            D3DFormat::R5G6B5,
            wgpu::Features::empty(),
            TextureUsageKind::Sampled,
        )
        .unwrap();
        assert!(r5g6b5.cpu_convert_to_bgra8);
        assert_eq!(r5g6b5.d3d_mip_level_pitch(7, 0), 7 * 2);
        assert_eq!(r5g6b5.d3d_mip_level_byte_len(7, 5, 0), 7 * 5 * 2);
        assert_eq!(r5g6b5.upload_bytes_per_row(7, 0), 7 * 4);
        assert_eq!(r5g6b5.upload_rows_per_image(5, 0), 5);
        assert_eq!(r5g6b5.upload_mip_level_byte_len(7, 5, 0), 7 * 5 * 4);

        // Native BC1 upload when supported.
        let bc1 = format_info(
            D3DFormat::Dxt1,
            wgpu::Features::TEXTURE_COMPRESSION_BC,
            TextureUsageKind::Sampled,
        )
        .unwrap();
        assert!(bc1.d3d_is_compressed);
        assert!(bc1.upload_is_compressed);
        assert!(!bc1.cpu_convert_to_bgra8);
        assert_eq!(bc1.d3d_mip_level_pitch(8, 0), 2 * 8);
        assert_eq!(bc1.d3d_mip_level_byte_len(8, 8, 0), 2 * 2 * 8);
        assert_eq!(bc1.d3d_mip_level_pitch(8, 1), 8);
        assert_eq!(bc1.d3d_mip_level_byte_len(8, 8, 1), 8);
        // 2x2 still occupies a full 4x4 block in D3D memory layout.
        assert_eq!(bc1.d3d_mip_level_pitch(8, 2), 8);
        assert_eq!(bc1.d3d_mip_level_byte_len(8, 8, 2), 8);
        assert_eq!(bc1.upload_bytes_per_row(8, 0), 2 * 8);
        assert_eq!(bc1.upload_rows_per_image(8, 0), 2);
        assert_eq!(bc1.upload_mip_level_byte_len(8, 8, 0), 2 * 2 * 8);

        // DXT5 fallback conversion path (upload is uncompressed, source/D3D layout is BC).
        let dxt5_fallback = format_info(
            D3DFormat::Dxt5,
            wgpu::Features::empty(),
            TextureUsageKind::Sampled,
        )
        .unwrap();
        assert!(dxt5_fallback.cpu_convert_to_bgra8);
        assert!(dxt5_fallback.d3d_is_compressed);
        assert!(!dxt5_fallback.upload_is_compressed);
        assert_eq!(dxt5_fallback.d3d_mip_level_pitch(7, 0), 2 * 16);
        assert_eq!(dxt5_fallback.d3d_mip_level_byte_len(7, 5, 0), 2 * 2 * 16);
        assert_eq!(dxt5_fallback.d3d_mip_level_pitch(7, 1), 16);
        assert_eq!(dxt5_fallback.d3d_mip_level_byte_len(7, 5, 1), 16);
        assert_eq!(dxt5_fallback.upload_bytes_per_row(7, 0), 7 * 4);
        assert_eq!(dxt5_fallback.upload_rows_per_image(5, 0), 5);
        assert_eq!(dxt5_fallback.upload_mip_level_byte_len(7, 5, 0), 7 * 5 * 4);
        assert_eq!(dxt5_fallback.upload_bytes_per_row(7, 1), 3 * 4);
        assert_eq!(dxt5_fallback.upload_rows_per_image(5, 1), 2);
        assert_eq!(dxt5_fallback.upload_mip_level_byte_len(7, 5, 1), 3 * 2 * 4);
    }

    #[test]
    fn align_copy_bytes_per_row_matches_wgpu_alignment_rules() {
        assert_eq!(align_copy_bytes_per_row(0), 0);
        assert_eq!(align_copy_bytes_per_row(1), 256);
        assert_eq!(align_copy_bytes_per_row(255), 256);
        assert_eq!(align_copy_bytes_per_row(256), 256);
        assert_eq!(align_copy_bytes_per_row(257), 512);
    }

    #[test]
    fn packed_16bit_formats_report_correct_lockrect_layout() {
        let features = wgpu::Features::empty();

        for fmt in [
            D3DFormat::R5G6B5,
            D3DFormat::A1R5G5B5,
            D3DFormat::X1R5G5B5,
            D3DFormat::A4R4G4B4,
        ] {
            let info = format_info(fmt, features, TextureUsageKind::Sampled).unwrap();
            assert_eq!(info.d3d_bytes_per_block, 2);
            assert_eq!(info.d3d_mip_level_pitch(3, 0), 6);
            assert_eq!(info.d3d_mip_level_byte_len(3, 2, 0), 12);

            // GPU storage uses BGRA8.
            assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
            assert_eq!(info.upload_bytes_per_block, 4);
            assert!(info.cpu_convert_to_bgra8);
        }
    }
}
