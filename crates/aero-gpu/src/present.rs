//! Presentation policy (color space + alpha mode) and framebuffer upload helpers.
//!
//! This module contains two related pieces:
//! - **Presentation policy** shared by presenter backends (WebGPU/WebGL2).
//! - A **dirty-rectangle based framebuffer uploader** (`Presenter`) that can reduce
//!   per-frame upload bandwidth by updating only changed regions.
//!
//! Keeping these in one module avoids cross-crate duplication while staying close to the
//! "presentation" boundary of the graphics stack.

use crate::dirty_rect::{merge_and_cap_rects, Rect};

// -----------------------------------------------------------------------------
// Presentation policy
// -----------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramebufferColorSpace {
    /// Texture contains linear values (typical for render targets).
    Linear,
    /// Texture contains sRGB-encoded values (rare for render targets; useful for upload paths).
    Srgb,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputColorSpace {
    /// Present without any gamma encoding.
    Linear,
    /// Present for an sRGB display (either via an sRGB surface format or shader encoding).
    Srgb,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentAlphaMode {
    /// Final output is treated as opaque; alpha is forced to 1.0.
    ///
    /// WebGPU: `alphaMode: "opaque"`
    /// WebGL2: context created with `{ alpha: false }`
    Opaque,
    /// Final output uses premultiplied alpha.
    ///
    /// WebGPU: `alphaMode: "premultiplied"` **and** the presenter must premultiply RGB.
    /// WebGL2: context created with `{ alpha: true, premultipliedAlpha: true }`.
    Premultiplied,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentOptions {
    pub framebuffer_color_space: FramebufferColorSpace,
    pub output_color_space: OutputColorSpace,
    pub alpha_mode: PresentAlphaMode,
}

impl Default for PresentOptions {
    fn default() -> Self {
        Self {
            framebuffer_color_space: FramebufferColorSpace::Linear,
            output_color_space: OutputColorSpace::Srgb,
            alpha_mode: PresentAlphaMode::Opaque,
        }
    }
}

/// Minimal surface-format model for selection logic.
///
/// The real implementation should use `wgpu::TextureFormat` (native + wasm targets) and/or
/// the WebGPU string formats, but the policy is the same:
/// **prefer an sRGB-capable surface for sRGB output**, otherwise fall back to linear + shader
/// encoding.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceFormat {
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgba8Unorm,
    Rgba8UnormSrgb,
}

impl SurfaceFormat {
    pub fn is_srgb(&self) -> bool {
        matches!(
            *self,
            SurfaceFormat::Bgra8UnormSrgb | SurfaceFormat::Rgba8UnormSrgb
        )
    }

    pub fn to_srgb(self) -> Option<Self> {
        match self {
            SurfaceFormat::Bgra8Unorm => Some(SurfaceFormat::Bgra8UnormSrgb),
            SurfaceFormat::Rgba8Unorm => Some(SurfaceFormat::Rgba8UnormSrgb),
            SurfaceFormat::Bgra8UnormSrgb | SurfaceFormat::Rgba8UnormSrgb => Some(self),
        }
    }
}

/// Choose the best surface format for presentation.
///
/// - If `output_color_space == Srgb`, pick an `*Srgb` format when available.
/// - Otherwise prefer the first provided format.
#[allow(dead_code)]
pub fn choose_surface_format(
    available: &[SurfaceFormat],
    output_color_space: OutputColorSpace,
) -> Option<SurfaceFormat> {
    if available.is_empty() {
        return None;
    }

    if output_color_space == OutputColorSpace::Srgb {
        // Prefer an sRGB surface, but keep the channel ordering stable (BGRA vs RGBA).
        if let Some(first) = available.first().copied() {
            if let Some(preferred_srgb) = first.to_srgb() {
                if available.contains(&preferred_srgb) {
                    return Some(preferred_srgb);
                }
            }
        }
        if let Some(any_srgb) = available.iter().copied().find(SurfaceFormat::is_srgb) {
            return Some(any_srgb);
        }
    }

    Some(available[0])
}

/// Whether the presenter must apply sRGB gamma encoding in the blit shader.
///
/// If the surface is already an sRGB surface, the GPU will encode automatically and the
/// shader must **not** apply gamma (to avoid double-encoding).
#[allow(dead_code)]
pub fn needs_srgb_encode_in_shader(
    output_color_space: OutputColorSpace,
    surface_format: SurfaceFormat,
) -> bool {
    output_color_space == OutputColorSpace::Srgb && !surface_format.is_srgb()
}

#[allow(dead_code)]
pub fn should_premultiply_in_shader(alpha_mode: PresentAlphaMode) -> bool {
    alpha_mode == PresentAlphaMode::Premultiplied
}

#[allow(dead_code)]
pub fn should_force_opaque_alpha(alpha_mode: PresentAlphaMode) -> bool {
    alpha_mode == PresentAlphaMode::Opaque
}

// -----------------------------------------------------------------------------
// Dirty-rectangle based framebuffer uploads
// -----------------------------------------------------------------------------

const DEFAULT_MAX_RECTS_PER_FRAME: usize = 128;

// Matches `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`.
const COPY_BYTES_PER_ROW_ALIGNMENT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentTelemetry {
    /// Number of rects requested by the caller (or generated by the diff engine / full-frame).
    pub rects_requested: usize,
    /// Rect count after merging overlaps/adjacency, before applying the cap.
    pub rects_after_merge: usize,
    /// Rect count that was actually uploaded (after applying the cap).
    pub rects_uploaded: usize,
    /// Total number of bytes submitted for upload this frame.
    pub bytes_uploaded: usize,
}

impl PresentTelemetry {
    #[must_use]
    pub fn merge_rate(self) -> f32 {
        if self.rects_requested == 0 {
            return 0.0;
        }
        let in_count = self.rects_requested as f32;
        let out_count = self.rects_uploaded as f32;
        (1.0 - (out_count / in_count)).clamp(0.0, 1.0)
    }
}

#[derive(Debug)]
pub enum PresentError {
    StrideTooSmall { stride: usize, min_stride: usize },
    FrameDataTooSmall { len: usize, min_len: usize },
    BytesPerRowTooLarge { bytes_per_row: usize },
}

impl std::fmt::Display for PresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StrideTooSmall { stride, min_stride } => write!(
                f,
                "stride too small (stride={stride}, min_stride={min_stride})"
            ),
            Self::FrameDataTooSmall { len, min_len } => {
                write!(f, "frame data too small (len={len}, min_len={min_len})")
            }
            Self::BytesPerRowTooLarge { bytes_per_row } => {
                write!(f, "bytes_per_row too large ({bytes_per_row})")
            }
        }
    }
}

impl std::error::Error for PresentError {}

/// Abstracts over the upload mechanism (e.g. `wgpu::Queue::write_texture`).
pub trait TextureWriter {
    /// Write `rect` into the destination texture.
    ///
    /// `data` is laid out as rows with stride `bytes_per_row`. For the last row, `data` only
    /// needs to contain the exact number of bytes for the copied region (the wgpu "required
    /// size" rule).
    fn write_texture(&mut self, rect: Rect, bytes_per_row: usize, data: &[u8]);
}

/// Presents a CPU framebuffer into a GPU texture, using dirty rectangles to minimize uploads.
pub struct Presenter<W> {
    width: u32,
    height: u32,
    bytes_per_pixel: usize,
    max_rects_per_frame: usize,
    writer: W,
    scratch: Vec<u8>,
    last_telemetry: PresentTelemetry,

    #[cfg(feature = "diff-engine")]
    diff: Option<crate::tile_diff::TileDiff>,
}

impl<W: TextureWriter> Presenter<W> {
    #[must_use]
    pub fn new(width: u32, height: u32, bytes_per_pixel: usize, writer: W) -> Self {
        Self {
            width,
            height,
            bytes_per_pixel,
            max_rects_per_frame: DEFAULT_MAX_RECTS_PER_FRAME,
            writer,
            scratch: Vec::new(),
            last_telemetry: PresentTelemetry {
                rects_requested: 0,
                rects_after_merge: 0,
                rects_uploaded: 0,
                bytes_uploaded: 0,
            },
            #[cfg(feature = "diff-engine")]
            diff: None,
        }
    }

    #[must_use]
    pub fn with_max_rects_per_frame(mut self, cap: usize) -> Self {
        self.max_rects_per_frame = cap;
        self
    }

    #[cfg(feature = "diff-engine")]
    pub fn enable_diff_engine(&mut self) {
        self.diff = Some(crate::tile_diff::TileDiff::new(
            self.width,
            self.height,
            self.bytes_per_pixel,
        ));
    }

    #[must_use]
    pub fn writer(&self) -> &W {
        &self.writer
    }

    #[allow(dead_code)]
    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    #[allow(dead_code)]
    pub fn into_writer(self) -> W {
        self.writer
    }

    #[must_use]
    pub fn last_telemetry(&self) -> PresentTelemetry {
        self.last_telemetry
    }

    /// Upload a frame into the destination texture.
    ///
    /// `stride` is the number of bytes between successive rows in `frame_data`.
    ///
    /// If `dirty` is `None`, the entire frame is uploaded (unless the optional diff engine is
    /// enabled, in which case a tile diff is used to generate dirty rectangles).
    pub fn present(
        &mut self,
        frame_data: &[u8],
        stride: usize,
        dirty: Option<&[Rect]>,
    ) -> Result<PresentTelemetry, PresentError> {
        let full_row_bytes = self
            .width
            .checked_mul(self.bytes_per_pixel as u32)
            .map(|v| v as usize)
            .unwrap_or(usize::MAX);

        if stride < full_row_bytes {
            return Err(PresentError::StrideTooSmall {
                stride,
                min_stride: full_row_bytes,
            });
        }

        let min_len = min_frame_len(stride, self.height as usize, full_row_bytes);
        if frame_data.len() < min_len {
            return Err(PresentError::FrameDataTooSmall {
                len: frame_data.len(),
                min_len,
            });
        }

        let (rects_requested, rects_input): (usize, Vec<Rect>) = match dirty {
            Some(rects) => (rects.len(), rects.to_vec()),
            None => {
                #[cfg(feature = "diff-engine")]
                if let Some(diff) = &mut self.diff {
                    let rects = diff.diff(frame_data, stride);
                    let requested = rects.len();
                    (requested, rects)
                } else {
                    (1, vec![Rect::new(0, 0, self.width, self.height)])
                }

                #[cfg(not(feature = "diff-engine"))]
                {
                    (1, vec![Rect::new(0, 0, self.width, self.height)])
                }
            }
        };

        let merged = merge_and_cap_rects(
            &rects_input,
            (self.width, self.height),
            self.max_rects_per_frame,
        );

        let mut bytes_uploaded = 0usize;
        for rect in &merged.rects {
            bytes_uploaded =
                bytes_uploaded.saturating_add(self.upload_rect(frame_data, stride, *rect)?);
        }

        let telemetry = PresentTelemetry {
            rects_requested,
            rects_after_merge: merged.rects_after_merge,
            rects_uploaded: merged.rects_after_cap,
            bytes_uploaded,
        };

        self.last_telemetry = telemetry;
        Ok(telemetry)
    }

    fn upload_rect(
        &mut self,
        frame_data: &[u8],
        stride: usize,
        rect: Rect,
    ) -> Result<usize, PresentError> {
        let row_bytes = rect
            .w
            .checked_mul(self.bytes_per_pixel as u32)
            .map(|v| v as usize)
            .unwrap_or(usize::MAX);

        let rect_h = rect.h as usize;
        if rect_h == 0 || row_bytes == 0 {
            return Ok(0);
        }

        let bytes_per_row = bytes_per_row_for_upload(row_bytes, rect_h);
        if bytes_per_row > u32::MAX as usize {
            return Err(PresentError::BytesPerRowTooLarge { bytes_per_row });
        }

        // Direct upload is only possible when we are uploading whole rows, otherwise we'd upload
        // the untouched bytes too (defeating the purpose of dirty rects).
        let can_direct = rect.x == 0
            && rect.w == self.width
            && stride == row_bytes
            && (rect_h == 1 || stride.is_multiple_of(COPY_BYTES_PER_ROW_ALIGNMENT));

        let required_len = required_data_len(bytes_per_row, row_bytes, rect_h);

        if can_direct {
            let offset = rect
                .y
                .checked_mul(stride as u32)
                .map(|v| v as usize)
                .unwrap_or(usize::MAX);
            let end = offset.saturating_add(required_len);
            let data = &frame_data[offset..end];
            self.writer.write_texture(rect, bytes_per_row, data);
            return Ok(required_len);
        }

        self.scratch.resize(required_len, 0);
        for row in 0..rect_h {
            let src_offset = (rect.y as usize + row)
                .saturating_mul(stride)
                .saturating_add(rect.x as usize * self.bytes_per_pixel);
            let dst_offset = row.saturating_mul(bytes_per_row);

            let src_end = src_offset.saturating_add(row_bytes);
            let dst_end = dst_offset.saturating_add(row_bytes);
            self.scratch[dst_offset..dst_end].copy_from_slice(&frame_data[src_offset..src_end]);
        }

        self.writer
            .write_texture(rect, bytes_per_row, &self.scratch);
        Ok(required_len)
    }
}

fn bytes_per_row_for_upload(row_bytes: usize, copy_height: usize) -> usize {
    if copy_height <= 1 {
        return row_bytes;
    }
    align_up(row_bytes, COPY_BYTES_PER_ROW_ALIGNMENT)
}

fn required_data_len(bytes_per_row: usize, row_bytes: usize, copy_height: usize) -> usize {
    if copy_height == 0 {
        return 0;
    }
    bytes_per_row
        .saturating_mul(copy_height.saturating_sub(1))
        .saturating_add(row_bytes)
}

fn min_frame_len(stride: usize, height: usize, row_bytes: usize) -> usize {
    if height == 0 {
        return 0;
    }
    stride
        .saturating_mul(height.saturating_sub(1))
        .saturating_add(row_bytes)
}

fn align_up(val: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (val + (align - 1)) & !(align - 1)
}

pub mod wgpu_writer {
    use super::{Rect, TextureWriter};

    /// `TextureWriter` implementation backed by `wgpu::Queue::write_texture`.
    ///
    /// The destination texture must be created with `COPY_DST` usage.
    #[allow(dead_code)]
    pub struct WgpuTextureWriter<'a> {
        pub queue: &'a wgpu::Queue,
        pub texture: &'a wgpu::Texture,
    }

    impl TextureWriter for WgpuTextureWriter<'_> {
        fn write_texture(&mut self, rect: Rect, bytes_per_row: usize, data: &[u8]) {
            let bytes_per_row_u32 = u32::try_from(bytes_per_row).expect("bytes_per_row overflow");

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: rect.x,
                        y: rect.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row_u32),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: rect.w,
                    height: rect.h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct WriteCall {
        rect: Rect,
        bytes_per_row: usize,
        data: Vec<u8>,
    }

    #[derive(Debug, Default)]
    struct RecordingWriter {
        calls: Vec<WriteCall>,
    }

    impl TextureWriter for RecordingWriter {
        fn write_texture(&mut self, rect: Rect, bytes_per_row: usize, data: &[u8]) {
            self.calls.push(WriteCall {
                rect,
                bytes_per_row,
                data: data.to_vec(),
            });
        }
    }

    fn patterned_bytes(len: usize) -> Vec<u8> {
        // Deterministic "unique-ish" pattern for verifying slices. Wraps naturally at 256.
        (0..len).map(|i| i as u8).collect()
    }

    #[test]
    fn direct_upload_path_full_frame_when_stride_is_aligned() {
        let (width, height, bpp) = (64u32, 4u32, 4usize);
        let stride = 256usize; // width*bpp == 256 -> 256-aligned.

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter.present(&frame_data, stride, None).unwrap();

        assert_eq!(
            telemetry,
            PresentTelemetry {
                rects_requested: 1,
                rects_after_merge: 1,
                rects_uploaded: 1,
                bytes_uploaded: frame_len,
            }
        );
        assert_eq!(presenter.last_telemetry(), telemetry);

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];

        assert_eq!(call.rect, Rect::new(0, 0, width, height));
        assert_eq!(call.bytes_per_row, stride);
        assert_eq!(call.data, frame_data);
    }

    #[test]
    fn scratch_upload_path_when_row_bytes_not_256_aligned() {
        let (width, height, bpp) = (3u32, 2u32, 4usize);
        let stride = 12usize; // width*bpp == 12 -> NOT 256-aligned for copy_height>1.

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter.present(&frame_data, stride, None).unwrap();

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];

        assert_eq!(call.rect, Rect::new(0, 0, width, height));
        assert_eq!(call.bytes_per_row, COPY_BYTES_PER_ROW_ALIGNMENT);

        let row_bytes = full_row_bytes;
        assert_eq!(
            call.data.len(),
            required_data_len(COPY_BYTES_PER_ROW_ALIGNMENT, row_bytes, height as usize)
        );

        // Row 0 copied into [0..row_bytes].
        assert_eq!(&call.data[0..row_bytes], &frame_data[0..row_bytes]);
        // Row 1 copied into [bytes_per_row..bytes_per_row+row_bytes].
        assert_eq!(
            &call.data[COPY_BYTES_PER_ROW_ALIGNMENT..(COPY_BYTES_PER_ROW_ALIGNMENT + row_bytes)],
            &frame_data[row_bytes..(row_bytes * 2)]
        );

        assert_eq!(telemetry.rects_uploaded, 1);
        assert_eq!(telemetry.bytes_uploaded, call.data.len());
    }

    #[test]
    fn scratch_upload_path_when_dirty_rect_is_not_full_width() {
        let (width, height, bpp) = (10u32, 10u32, 4usize);
        let stride = 40usize; // width*bpp == 40

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let dirty = Rect::new(2, 3, 4, 2);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter
            .present(&frame_data, stride, Some(std::slice::from_ref(&dirty)))
            .unwrap();

        assert_eq!(telemetry.rects_requested, 1);
        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];

        assert_eq!(call.rect, dirty);
        assert_eq!(call.bytes_per_row, COPY_BYTES_PER_ROW_ALIGNMENT);

        let row_bytes = dirty.w as usize * bpp;
        assert_eq!(
            call.data.len(),
            required_data_len(COPY_BYTES_PER_ROW_ALIGNMENT, row_bytes, dirty.h as usize)
        );

        let row0_src_offset = dirty.y as usize * stride + dirty.x as usize * bpp;
        let row1_src_offset = (dirty.y as usize + 1) * stride + dirty.x as usize * bpp;

        assert_eq!(
            &call.data[0..row_bytes],
            &frame_data[row0_src_offset..(row0_src_offset + row_bytes)]
        );
        assert_eq!(
            &call.data[COPY_BYTES_PER_ROW_ALIGNMENT..(COPY_BYTES_PER_ROW_ALIGNMENT + row_bytes)],
            &frame_data[row1_src_offset..(row1_src_offset + row_bytes)]
        );

        assert_eq!(telemetry.bytes_uploaded, call.data.len());
    }

    #[test]
    fn error_stride_too_small() {
        let (width, height, bpp) = (10u32, 10u32, 4usize);
        let stride = 39usize; // < width*bpp (40)

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let err = presenter.present(&[], stride, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "stride too small (stride=39, min_stride=40)"
        );
        assert!(matches!(
            err,
            PresentError::StrideTooSmall {
                stride: 39,
                min_stride: 40
            }
        ));
    }

    #[test]
    fn error_frame_data_too_small() {
        let (width, height, bpp) = (10u32, 10u32, 4usize);
        let stride = 40usize; // == width*bpp

        let full_row_bytes = width as usize * bpp;
        let min_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(min_len - 1);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let err = presenter.present(&frame_data, stride, None).unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "frame data too small (len={}, min_len={})",
                min_len - 1,
                min_len
            )
        );
        assert!(matches!(
            err,
            PresentError::FrameDataTooSmall {
                len,
                min_len: ml
            } if len == min_len - 1 && ml == min_len
        ));
    }

    #[test]
    fn uploads_multiple_height1_rects_and_telemetry_sums_bytes() {
        let (width, height, bpp) = (10u32, 3u32, 4usize);
        let stride = 40usize;

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let r1 = Rect::new(1, 0, 2, 1);
        let r2 = Rect::new(5, 2, 3, 1);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter.present(&frame_data, stride, Some(&[r1, r2])).unwrap();

        assert_eq!(telemetry.rects_requested, 2);
        assert_eq!(telemetry.rects_after_merge, 2);
        assert_eq!(telemetry.rects_uploaded, 2);

        assert_eq!(presenter.writer().calls.len(), 2);

        // merge_and_cap_rects sorts by (y, x) -> r1 then r2.
        let call1 = &presenter.writer().calls[0];
        let call2 = &presenter.writer().calls[1];

        assert_eq!(call1.rect, r1);
        assert_eq!(call2.rect, r2);

        let r1_row_bytes = r1.w as usize * bpp;
        let r2_row_bytes = r2.w as usize * bpp;

        // Height=1 doesn't require 256-byte alignment.
        assert_eq!(call1.bytes_per_row, r1_row_bytes);
        assert_eq!(call2.bytes_per_row, r2_row_bytes);

        let r1_src = r1.y as usize * stride + r1.x as usize * bpp;
        let r2_src = r2.y as usize * stride + r2.x as usize * bpp;

        assert_eq!(
            call1.data,
            frame_data[r1_src..(r1_src + r1_row_bytes)].to_vec()
        );
        assert_eq!(
            call2.data,
            frame_data[r2_src..(r2_src + r2_row_bytes)].to_vec()
        );

        let sum = call1.data.len() + call2.data.len();
        assert_eq!(telemetry.bytes_uploaded, sum);
        assert_eq!(telemetry.merge_rate(), 0.0);
    }

    #[test]
    fn merges_overlapping_dirty_rects_and_reports_telemetry() {
        let (width, height, bpp) = (20u32, 20u32, 4usize);
        let stride = 80usize;

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let r1 = Rect::new(0, 0, 10, 10);
        let r2 = Rect::new(5, 5, 10, 10);
        let merged = Rect::new(0, 0, 15, 15);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter.present(&frame_data, stride, Some(&[r1, r2])).unwrap();

        assert_eq!(telemetry.rects_requested, 2);
        assert_eq!(telemetry.rects_after_merge, 1);
        assert_eq!(telemetry.rects_uploaded, 1);
        assert_eq!(telemetry.merge_rate(), 0.5);

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];
        assert_eq!(call.rect, merged);
        assert_eq!(call.bytes_per_row, COPY_BYTES_PER_ROW_ALIGNMENT);

        let row_bytes = merged.w as usize * bpp;
        let rect_h = merged.h as usize;
        let expected_len = required_data_len(COPY_BYTES_PER_ROW_ALIGNMENT, row_bytes, rect_h);
        assert_eq!(call.data.len(), expected_len);
        assert_eq!(telemetry.bytes_uploaded, expected_len);

        // Spot-check a couple rows to ensure the merged rect is copied correctly.
        let row0_src = 0 * stride;
        let row_last_src = (rect_h - 1) * stride;
        let last_row_dst = (rect_h - 1) * COPY_BYTES_PER_ROW_ALIGNMENT;

        assert_eq!(&call.data[0..row_bytes], &frame_data[row0_src..row0_src + row_bytes]);
        assert_eq!(
            &call.data[last_row_dst..(last_row_dst + row_bytes)],
            &frame_data[row_last_src..row_last_src + row_bytes]
        );
    }

    #[test]
    fn caps_rect_count_and_uploads_bounding_box() {
        let (width, height, bpp) = (10u32, 10u32, 4usize);
        let stride = 40usize;

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let r1 = Rect::new(0, 0, 1, 1);
        let r2 = Rect::new(9, 9, 1, 1);
        let capped = Rect::new(0, 0, width, height);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default())
            .with_max_rects_per_frame(1);
        let telemetry = presenter.present(&frame_data, stride, Some(&[r1, r2])).unwrap();

        assert_eq!(telemetry.rects_requested, 2);
        assert_eq!(telemetry.rects_after_merge, 2);
        assert_eq!(telemetry.rects_uploaded, 1);
        assert_eq!(telemetry.merge_rate(), 0.5);

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];
        assert_eq!(call.rect, capped);
        assert_eq!(call.bytes_per_row, COPY_BYTES_PER_ROW_ALIGNMENT);

        let expected_len =
            required_data_len(COPY_BYTES_PER_ROW_ALIGNMENT, full_row_bytes, height as usize);
        assert_eq!(call.data.len(), expected_len);
        assert_eq!(telemetry.bytes_uploaded, expected_len);

        // Spot-check that padding in the source (none here) and destination layout are handled.
        assert_eq!(&call.data[0..full_row_bytes], &frame_data[0..full_row_bytes]);
        let last_row_dst = (height as usize - 1) * COPY_BYTES_PER_ROW_ALIGNMENT;
        let last_row_src = (height as usize - 1) * stride;
        assert_eq!(
            &call.data[last_row_dst..last_row_dst + full_row_bytes],
            &frame_data[last_row_src..last_row_src + full_row_bytes]
        );
    }

    #[test]
    fn direct_upload_path_full_width_rect_with_y_offset_uses_correct_slice() {
        let (width, height, bpp) = (64u32, 4u32, 4usize);
        let stride = 256usize;

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let rect = Rect::new(0, 1, width, 2);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter
            .present(&frame_data, stride, Some(std::slice::from_ref(&rect)))
            .unwrap();

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];

        assert_eq!(call.rect, rect);
        assert_eq!(call.bytes_per_row, stride);

        let expected_len = required_data_len(stride, full_row_bytes, rect.h as usize);
        let expected_off = rect.y as usize * stride;

        assert_eq!(call.data.len(), expected_len);
        assert_eq!(
            call.data,
            frame_data[expected_off..(expected_off + expected_len)].to_vec()
        );

        assert_eq!(telemetry.rects_requested, 1);
        assert_eq!(telemetry.rects_uploaded, 1);
        assert_eq!(telemetry.bytes_uploaded, expected_len);
    }

    #[test]
    fn direct_upload_path_height1_does_not_require_256_alignment() {
        let (width, height, bpp) = (3u32, 1u32, 4usize);
        let stride = 12usize; // not 256-aligned, but height=1 allows direct upload.

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter.present(&frame_data, stride, None).unwrap();

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];

        assert_eq!(call.rect, Rect::new(0, 0, width, height));
        assert_eq!(call.bytes_per_row, full_row_bytes);
        assert_eq!(call.data, frame_data);

        assert_eq!(telemetry.bytes_uploaded, full_row_bytes);
    }

    #[test]
    fn scratch_upload_path_when_stride_has_padding_even_for_full_frame() {
        let (width, height, bpp) = (64u32, 4u32, 4usize);
        let row_bytes = width as usize * bpp; // 256
        let stride = row_bytes + 4; // padded rows

        let frame_len = min_frame_len(stride, height as usize, row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter.present(&frame_data, stride, None).unwrap();

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];

        assert_eq!(call.rect, Rect::new(0, 0, width, height));
        assert_eq!(call.bytes_per_row, row_bytes);

        let expected_len = required_data_len(row_bytes, row_bytes, height as usize);
        assert_eq!(call.data.len(), expected_len);
        assert_eq!(telemetry.bytes_uploaded, expected_len);

        for row in 0..height as usize {
            let src_off = row * stride;
            let dst_off = row * row_bytes;
            assert_eq!(
                &call.data[dst_off..dst_off + row_bytes],
                &frame_data[src_off..src_off + row_bytes]
            );
        }
    }

    #[test]
    fn present_with_empty_dirty_rects_uploads_nothing_and_merge_rate_is_zero() {
        let (width, height, bpp) = (10u32, 10u32, 4usize);
        let stride = 40usize;

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter.present(&frame_data, stride, Some(&[])).unwrap();

        assert_eq!(presenter.writer().calls.len(), 0);
        assert_eq!(
            telemetry,
            PresentTelemetry {
                rects_requested: 0,
                rects_after_merge: 0,
                rects_uploaded: 0,
                bytes_uploaded: 0,
            }
        );
        assert_eq!(telemetry.merge_rate(), 0.0);
        assert_eq!(presenter.last_telemetry(), telemetry);
    }

    #[test]
    fn present_clamps_out_of_bounds_dirty_rect_before_uploading() {
        let (width, height, bpp) = (10u32, 10u32, 4usize);
        let stride = 40usize;

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let requested = Rect::new(8, 8, 10, 10);
        let clamped = Rect::new(8, 8, 2, 2);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter
            .present(&frame_data, stride, Some(std::slice::from_ref(&requested)))
            .unwrap();

        assert_eq!(telemetry.rects_requested, 1);
        assert_eq!(telemetry.rects_uploaded, 1);

        assert_eq!(presenter.writer().calls.len(), 1);
        let call = &presenter.writer().calls[0];

        assert_eq!(call.rect, clamped);
        assert_eq!(call.bytes_per_row, COPY_BYTES_PER_ROW_ALIGNMENT);

        let row_bytes = clamped.w as usize * bpp;
        let expected_len =
            required_data_len(COPY_BYTES_PER_ROW_ALIGNMENT, row_bytes, clamped.h as usize);
        assert_eq!(call.data.len(), expected_len);
        assert_eq!(telemetry.bytes_uploaded, expected_len);

        let row0_src = clamped.y as usize * stride + clamped.x as usize * bpp;
        let row1_src = (clamped.y as usize + 1) * stride + clamped.x as usize * bpp;

        assert_eq!(
            &call.data[0..row_bytes],
            &frame_data[row0_src..row0_src + row_bytes]
        );
        assert_eq!(
            &call.data[COPY_BYTES_PER_ROW_ALIGNMENT..COPY_BYTES_PER_ROW_ALIGNMENT + row_bytes],
            &frame_data[row1_src..row1_src + row_bytes]
        );
    }

    #[test]
    fn present_drops_fully_out_of_bounds_dirty_rect() {
        let (width, height, bpp) = (10u32, 10u32, 4usize);
        let stride = 40usize;

        let full_row_bytes = width as usize * bpp;
        let frame_len = min_frame_len(stride, height as usize, full_row_bytes);
        let frame_data = patterned_bytes(frame_len);

        let requested = Rect::new(100, 100, 10, 10);

        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default());
        let telemetry = presenter
            .present(&frame_data, stride, Some(std::slice::from_ref(&requested)))
            .unwrap();

        assert_eq!(presenter.writer().calls.len(), 0);
        assert_eq!(
            telemetry,
            PresentTelemetry {
                rects_requested: 1,
                rects_after_merge: 0,
                rects_uploaded: 0,
                bytes_uploaded: 0,
            }
        );
        assert_eq!(telemetry.merge_rate(), 1.0);
    }

    #[test]
    fn error_bytes_per_row_too_large_when_alignment_exceeds_u32_max() {
        // This test calls the internal helper directly to avoid allocating a multi-GB frame
        // buffer. The error is raised before any frame_data indexing occurs.
        let mut presenter = Presenter::new(1, 1, 1, RecordingWriter::default());
        let rect = Rect::new(0, 0, u32::MAX, 2);

        let err = presenter.upload_rect(&[], 0, rect).unwrap_err();
        assert_eq!(
            err.to_string(),
            "bytes_per_row too large (4294967296)"
        );
        assert!(matches!(
            err,
            PresentError::BytesPerRowTooLarge {
                bytes_per_row: 4294967296
            }
        ));
        assert_eq!(presenter.writer().calls.len(), 0);
    }
}
