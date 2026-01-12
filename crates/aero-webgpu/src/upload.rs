//! Helpers for staging uploads (CPU → GPU).
//!
//! This module is intentionally small; we can evolve it into a more general
//! staging belt / suballocator once the graphics stack starts streaming larger
//! dynamic resources.

/// A reusable scratch buffer for uploading RGBA8 pixel data to a 2D texture.
#[derive(Default)]
pub struct Rgba8TextureUploader {
    staging: Vec<u8>,
}

impl Rgba8TextureUploader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upload an RGBA8 byte slice (`width * height * 4`) into `texture`.
    ///
    /// The destination texture must be created with `COPY_DST` usage.
    ///
    /// Note: WebGPU requires `bytes_per_row` to be a multiple of 256. This helper
    /// avoids the extra copy when `width * 4` is already aligned, otherwise it
    /// packs rows into an internal staging buffer with padding.
    pub fn write_texture(
        &mut self,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        self.write_texture_with_stride(queue, texture, width, height, rgba, width * 4);
    }

    /// Upload a strided RGBA8 framebuffer to `texture`.
    ///
    /// `stride_bytes` must be at least `width * 4`. If it is also 256-byte aligned,
    /// the upload can avoid the repack copy and write directly from `rgba`.
    pub fn write_texture_with_stride(
        &mut self,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        rgba: &[u8],
        stride_bytes: u32,
    ) {
        // Treat empty uploads as a no-op. WebGPU validation rejects zero-sized copy extents, and
        // callers may produce degenerate frame sizes during resize/minimize transitions.
        if width == 0 || height == 0 {
            return;
        }

        let unpadded_bpr = width * 4;
        debug_assert!(stride_bytes >= unpadded_bpr);

        let required_len = (stride_bytes as usize) * (height as usize);
        debug_assert!(rgba.len() >= required_len);

        let aligned = stride_bytes.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

        let (bytes, bytes_per_row) = if aligned {
            (&rgba[..required_len], stride_bytes)
        } else {
            let padded_bpr = padded_bytes_per_row(unpadded_bpr);
            copy_rgba8_to_padded_strided(
                rgba,
                width,
                height,
                stride_bytes,
                padded_bpr,
                &mut self.staging,
            );
            (&self.staging[..], padded_bpr)
        };

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
}

fn padded_bytes_per_row(unpadded_bytes_per_row: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded_bytes_per_row.div_ceil(align) * align
}

fn copy_rgba8_to_padded_strided(
    rgba: &[u8],
    width: u32,
    height: u32,
    src_stride: u32,
    padded_bpr: u32,
    out: &mut Vec<u8>,
) {
    debug_assert!(src_stride >= width.saturating_mul(4));
    debug_assert!((rgba.len() as u64) >= (src_stride as u64) * (height as u64));
    debug_assert!(padded_bpr.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));

    let unpadded_bpr = width * 4;
    debug_assert!(padded_bpr >= unpadded_bpr);

    let padded_len = padded_bpr as usize * height as usize;
    out.resize(padded_len, 0);

    for y in 0..height as usize {
        let src_off = y * src_stride as usize;
        let src_row = &rgba[src_off..src_off + unpadded_bpr as usize];
        let dst_row =
            &mut out[y * padded_bpr as usize..y * padded_bpr as usize + unpadded_bpr as usize];
        dst_row.copy_from_slice(src_row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_padded_bytes_per_row_alignment() {
        assert_eq!(padded_bytes_per_row(0), 0);
        assert_eq!(padded_bytes_per_row(4), 256);
        assert_eq!(padded_bytes_per_row(256), 256);
        assert_eq!(padded_bytes_per_row(257), 512);
    }

    #[test]
    fn test_copy_rgba8_to_padded_packs_rows() {
        let width = 3u32;
        let height = 2u32;
        let rgba: Vec<u8> = (0..(width * height * 4)).map(|v| v as u8).collect();

        let padded_bpr = padded_bytes_per_row(width * 4);
        assert_eq!(padded_bpr, 256);

        let mut out = Vec::new();
        copy_rgba8_to_padded_strided(&rgba, width, height, width * 4, padded_bpr, &mut out);

        assert_eq!(out.len(), padded_bpr as usize * height as usize);
        assert_eq!(&out[..(width * 4) as usize], &rgba[..(width * 4) as usize]);

        let second_row_offset = padded_bpr as usize;
        assert_eq!(
            &out[second_row_offset..second_row_offset + (width * 4) as usize],
            &rgba[(width * 4) as usize..]
        );
    }
}
