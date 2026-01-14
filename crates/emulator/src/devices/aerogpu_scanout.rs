use memory::MemoryBus;

pub use aero_devices_gpu::AeroGpuFormat;
use aero_shared::scanout_state::{ScanoutStateUpdate, SCANOUT_FORMAT_B8G8R8X8};

// -----------------------------------------------------------------------------
// Defensive caps (host readback paths)
// -----------------------------------------------------------------------------
//
// `AeroGpu*Config::read_rgba` reads guest-controlled scanout/cursor bitmaps and returns a host-owned
// `Vec<u8>` containing RGBA8 pixels. Since the guest controls width/height/pitch, these helpers
// must not allocate unbounded memory.
const MAX_HOST_SCANOUT_RGBA8888_BYTES: usize = 64 * 1024 * 1024; // 16,777,216 pixels (~4K@32bpp)
const MAX_HOST_CURSOR_RGBA8888_BYTES: usize = 4 * 1024 * 1024; // 1,048,576 pixels (~1024x1024)

// Values derived from the canonical `aero-protocol` definition of `enum aerogpu_format`.
//
// Format semantics (mirrors `drivers/aerogpu/protocol/aerogpu_pci.h` and
// `docs/16-gpu-command-abi.md` §2.5.1):
// - `*X8*` formats (`B8G8R8X8*`, `R8G8B8X8*`) do not carry alpha. When converting to RGBA for
//   scanout/cursor presentation or blending, treat alpha as fully opaque (`A=0xFF`) and ignore the
//   stored `X` byte.
// - `*_SRGB` formats are byte-layout-identical to their UNORM counterparts; only the color space
//   interpretation differs (sampling decodes sRGB→linear; render-target writes may encode
//   linear→sRGB). Presenters must avoid double-applying gamma.

#[derive(Clone, Debug)]
pub struct AeroGpuScanoutConfig {
    pub enable: bool,
    pub width: u32,
    pub height: u32,
    pub format: AeroGpuFormat,
    pub pitch_bytes: u32,
    pub fb_gpa: u64,
}

impl Default for AeroGpuScanoutConfig {
    fn default() -> Self {
        Self {
            enable: false,
            width: 0,
            height: 0,
            format: AeroGpuFormat::Invalid,
            pitch_bytes: 0,
            fb_gpa: 0,
        }
    }
}

impl AeroGpuScanoutConfig {
    fn disabled_scanout_state_update(source: u32) -> ScanoutStateUpdate {
        ScanoutStateUpdate {
            source,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            // Keep format at a stable default even while disabled.
            format: SCANOUT_FORMAT_B8G8R8X8,
        }
    }

    /// Convert this scanout register block into a shared [`ScanoutStateUpdate`].
    ///
    /// If the configuration is disabled or invalid (including unsupported pixel formats), this
    /// returns a "disabled" descriptor with `width/height/base_paddr/pitch = 0`.
    pub fn to_scanout_state_update(&self, source: u32) -> ScanoutStateUpdate {
        if !self.enable {
            return Self::disabled_scanout_state_update(source);
        }

        let width = self.width;
        let height = self.height;
        if width == 0 || height == 0 {
            return Self::disabled_scanout_state_update(source);
        }

        let fb_gpa = self.fb_gpa;
        if fb_gpa == 0 {
            return Self::disabled_scanout_state_update(source);
        }

        // Scanout state supports only the packed pixel formats that scanout consumers can present
        // deterministically today.
        let Some(bytes_per_pixel) = self.format.bytes_per_pixel() else {
            return Self::disabled_scanout_state_update(source);
        };
        let format = self.format as u32;

        // Validate pitch >= width*bytes_per_pixel and that address arithmetic doesn't overflow.
        let row_bytes = u64::from(width).checked_mul(bytes_per_pixel as u64);
        let Some(row_bytes) = row_bytes else {
            return Self::disabled_scanout_state_update(source);
        };
        let pitch = u64::from(self.pitch_bytes);
        if pitch < row_bytes {
            return Self::disabled_scanout_state_update(source);
        }
        if bytes_per_pixel != 0 && pitch % (bytes_per_pixel as u64) != 0 {
            // Scanout consumers treat the pitch as a byte stride for `bytes_per_pixel`-sized pixels.
            // If it's not a multiple of the pixel size, row starts would land mid-pixel.
            return Self::disabled_scanout_state_update(source);
        }

        // Ensure `fb_gpa + (height-1)*pitch + row_bytes` does not overflow.
        let Some(last_row_offset) = u64::from(height)
            .checked_sub(1)
            .and_then(|rows| rows.checked_mul(pitch))
        else {
            return Self::disabled_scanout_state_update(source);
        };
        let Some(end_offset) = last_row_offset.checked_add(row_bytes) else {
            return Self::disabled_scanout_state_update(source);
        };
        if fb_gpa.checked_add(end_offset).is_none() {
            return Self::disabled_scanout_state_update(source);
        }

        ScanoutStateUpdate {
            source,
            base_paddr_lo: fb_gpa as u32,
            base_paddr_hi: (fb_gpa >> 32) as u32,
            width,
            height,
            pitch_bytes: self.pitch_bytes,
            format,
        }
    }

    pub fn read_rgba(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u8>> {
        if !self.enable {
            return None;
        }
        let bytes_per_pixel = self.format.bytes_per_pixel()?;
        let width = usize::try_from(self.width).ok()?;
        let height = usize::try_from(self.height).ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        if self.fb_gpa == 0 {
            return None;
        }
        let pitch = usize::try_from(self.pitch_bytes).ok()?;
        let row_bytes = width.checked_mul(bytes_per_pixel)?;
        if pitch < row_bytes {
            return None;
        }

        // Validate GPA arithmetic does not wrap.
        let pitch_u64 = u64::from(self.pitch_bytes);
        let row_bytes_u64 = u64::try_from(row_bytes).ok()?;
        let last_row_gpa = self
            .fb_gpa
            .checked_add((height as u64).checked_sub(1)?.checked_mul(pitch_u64)?)?;
        last_row_gpa.checked_add(row_bytes_u64)?;

        let out_len = width.checked_mul(height)?.checked_mul(4)?;
        if out_len > MAX_HOST_SCANOUT_RGBA8888_BYTES {
            return None;
        }
        let mut out = vec![0u8; out_len];
        let mut row_buf = vec![0u8; row_bytes];

        for y in 0..height {
            let row_gpa = self.fb_gpa + (y as u64) * pitch_u64;
            mem.read_physical(row_gpa, &mut row_buf);
            let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];

            match self.format {
                AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8A8UnormSrgb => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = src[3];
                    }
                }
                AeroGpuFormat::B8G8R8X8Unorm | AeroGpuFormat::B8G8R8X8UnormSrgb => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = 0xff;
                    }
                }
                AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8A8UnormSrgb => {
                    dst_row.copy_from_slice(&row_buf);
                }
                AeroGpuFormat::R8G8B8X8Unorm | AeroGpuFormat::R8G8B8X8UnormSrgb => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[0];
                        dst[1] = src[1];
                        dst[2] = src[2];
                        dst[3] = 0xff;
                    }
                }
                AeroGpuFormat::B5G6R5Unorm => {
                    for x in 0..width {
                        let pix = u16::from_le_bytes([row_buf[x * 2], row_buf[x * 2 + 1]]);
                        let b = (pix & 0x1f) as u8;
                        let g = ((pix >> 5) & 0x3f) as u8;
                        let r = ((pix >> 11) & 0x1f) as u8;
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = (r << 3) | (r >> 2);
                        dst[1] = (g << 2) | (g >> 4);
                        dst[2] = (b << 3) | (b >> 2);
                        dst[3] = 0xff;
                    }
                }
                AeroGpuFormat::B5G5R5A1Unorm => {
                    for x in 0..width {
                        let pix = u16::from_le_bytes([row_buf[x * 2], row_buf[x * 2 + 1]]);
                        let b = (pix & 0x1f) as u8;
                        let g = ((pix >> 5) & 0x1f) as u8;
                        let r = ((pix >> 10) & 0x1f) as u8;
                        let a = ((pix >> 15) & 0x1) as u8;
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = (r << 3) | (r >> 2);
                        dst[1] = (g << 3) | (g >> 2);
                        dst[2] = (b << 3) | (b >> 2);
                        dst[3] = if a != 0 { 0xff } else { 0x00 };
                    }
                }
                _ => return None,
            }
        }

        Some(out)
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuCursorConfig {
    pub enable: bool,
    pub x: i32,
    pub y: i32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub width: u32,
    pub height: u32,
    pub format: AeroGpuFormat,
    pub fb_gpa: u64,
    pub pitch_bytes: u32,
}

impl Default for AeroGpuCursorConfig {
    fn default() -> Self {
        Self {
            enable: false,
            x: 0,
            y: 0,
            hot_x: 0,
            hot_y: 0,
            width: 0,
            height: 0,
            format: AeroGpuFormat::Invalid,
            fb_gpa: 0,
            pitch_bytes: 0,
        }
    }
}

impl AeroGpuCursorConfig {
    pub fn read_rgba(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u8>> {
        if !self.enable {
            return None;
        }

        let bytes_per_pixel = self.format.bytes_per_pixel()?;
        if bytes_per_pixel != 4 {
            // MVP: only support 32bpp cursor formats.
            return None;
        }

        let width = usize::try_from(self.width).ok()?;
        let height = usize::try_from(self.height).ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        if self.fb_gpa == 0 {
            return None;
        }

        let pitch = usize::try_from(self.pitch_bytes).ok()?;
        let row_bytes = width.checked_mul(bytes_per_pixel)?;
        if pitch < row_bytes {
            return None;
        }

        // Validate GPA arithmetic does not wrap.
        let pitch_u64 = u64::from(self.pitch_bytes);
        let row_bytes_u64 = u64::try_from(row_bytes).ok()?;
        let last_row_gpa = self
            .fb_gpa
            .checked_add((height as u64).checked_sub(1)?.checked_mul(pitch_u64)?)?;
        last_row_gpa.checked_add(row_bytes_u64)?;

        let out_len = width.checked_mul(height)?.checked_mul(4)?;
        if out_len > MAX_HOST_CURSOR_RGBA8888_BYTES {
            return None;
        }
        let mut out = vec![0u8; out_len];
        let mut row_buf = vec![0u8; row_bytes];

        for y in 0..height {
            let row_gpa = self.fb_gpa + (y as u64) * (self.pitch_bytes as u64);
            mem.read_physical(row_gpa, &mut row_buf);
            let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];

            match self.format {
                AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8A8UnormSrgb => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = src[3];
                    }
                }
                AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8A8UnormSrgb => {
                    dst_row.copy_from_slice(&row_buf);
                }
                // Cursor should be ARGB, but accept XRGB for now (opaque alpha) so
                // diagnostics/debug cursors are visible even if the guest picks X8R8G8B8.
                AeroGpuFormat::B8G8R8X8Unorm | AeroGpuFormat::B8G8R8X8UnormSrgb => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = 0xff;
                    }
                }
                AeroGpuFormat::R8G8B8X8Unorm | AeroGpuFormat::R8G8B8X8UnormSrgb => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[0];
                        dst[1] = src[1];
                        dst[2] = src[2];
                        dst[3] = 0xff;
                    }
                }
                _ => return None,
            }
        }

        Some(out)
    }
}

pub fn composite_cursor_rgba_over_scanout(
    scanout_rgba: &mut [u8],
    scanout_width: usize,
    scanout_height: usize,
    cursor: &AeroGpuCursorConfig,
    cursor_rgba: &[u8],
) -> Option<()> {
    if !cursor.enable {
        return Some(());
    }

    let scanout_len = scanout_width.checked_mul(scanout_height)?.checked_mul(4)?;
    if scanout_rgba.len() < scanout_len {
        return None;
    }

    let cursor_width = usize::try_from(cursor.width).ok()?;
    let cursor_height = usize::try_from(cursor.height).ok()?;
    if cursor_width == 0 || cursor_height == 0 {
        return Some(());
    }

    let cursor_len = cursor_width.checked_mul(cursor_height)?.checked_mul(4)?;
    if cursor_rgba.len() < cursor_len {
        return None;
    }

    let origin_x = i64::from(cursor.x).checked_sub(i64::from(cursor.hot_x))?;
    let origin_y = i64::from(cursor.y).checked_sub(i64::from(cursor.hot_y))?;

    for cy in 0..cursor_height {
        let dst_y = origin_y + cy as i64;
        if dst_y < 0 || dst_y >= scanout_height as i64 {
            continue;
        }
        for cx in 0..cursor_width {
            let dst_x = origin_x + cx as i64;
            if dst_x < 0 || dst_x >= scanout_width as i64 {
                continue;
            }

            let src_off = (cy * cursor_width + cx) * 4;
            let src_a = cursor_rgba[src_off + 3];
            if src_a == 0 {
                continue;
            }

            let dst_off = (dst_y as usize * scanout_width + dst_x as usize) * 4;
            if src_a == 0xff {
                scanout_rgba[dst_off..dst_off + 4]
                    .copy_from_slice(&cursor_rgba[src_off..src_off + 4]);
                continue;
            }

            let inv_a = 255u16 - src_a as u16;
            for ch in 0..3 {
                let src = cursor_rgba[src_off + ch] as u16;
                let dst = scanout_rgba[dst_off + ch] as u16;
                let blended = src * src_a as u16 + dst * inv_a;
                scanout_rgba[dst_off + ch] = ((blended + 127) / 255) as u8;
            }

            scanout_rgba[dst_off + 3] = 0xff;
        }
    }

    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_shared::scanout_state::{
        SCANOUT_FORMAT_B5G5R5A1, SCANOUT_FORMAT_B5G6R5, SCANOUT_FORMAT_B8G8R8A8,
        SCANOUT_FORMAT_B8G8R8A8_SRGB, SCANOUT_FORMAT_B8G8R8X8_SRGB, SCANOUT_SOURCE_WDDM,
    };

    #[derive(Clone, Debug)]
    struct VecMemory {
        data: Vec<u8>,
    }

    impl VecMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
            let start = usize::try_from(paddr).expect("paddr too large");
            let end = start.checked_add(len).expect("address wrap");
            assert!(end <= self.data.len(), "out-of-bounds physical access");
            start..end
        }
    }

    impl MemoryBus for VecMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let range = self.range(paddr, buf.len());
            buf.copy_from_slice(&self.data[range]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let range = self.range(paddr, buf.len());
            self.data[range].copy_from_slice(buf);
        }
    }

    #[test]
    fn cursor_read_bgra_and_rgba() {
        let mut mem = VecMemory::new(0x1000);
        let fb_gpa = 0x100u64;

        // 2x1 pixels.
        // - BGRA: (R=1,G=2,B=3,A=4), (R=10,G=20,B=30,A=40)
        mem.write_physical(fb_gpa, &[3, 2, 1, 4, 30, 20, 10, 40]);

        let mut cfg = AeroGpuCursorConfig {
            enable: true,
            width: 2,
            height: 1,
            pitch_bytes: 8,
            fb_gpa,
            format: AeroGpuFormat::B8G8R8A8Unorm,
            ..Default::default()
        };
        assert_eq!(
            cfg.read_rgba(&mut mem).unwrap(),
            vec![1, 2, 3, 4, 10, 20, 30, 40]
        );

        cfg.format = AeroGpuFormat::R8G8B8A8Unorm;
        // Write RGBA directly for the same pixels.
        mem.write_physical(fb_gpa, &[1, 2, 3, 4, 10, 20, 30, 40]);
        assert_eq!(
            cfg.read_rgba(&mut mem).unwrap(),
            vec![1, 2, 3, 4, 10, 20, 30, 40]
        );
    }

    #[test]
    fn scanout_read_rgba_rejects_overflowing_fb_gpa() {
        let mut mem = VecMemory::new(0x1000);

        // Force `fb_gpa + (height-1)*pitch_bytes` overflow while keeping the scanout dimensions
        // otherwise valid.
        let cfg = AeroGpuScanoutConfig {
            enable: true,
            width: 1,
            height: 2,
            pitch_bytes: 4,
            fb_gpa: u64::MAX - 1,
            format: AeroGpuFormat::R8G8B8A8Unorm,
        };

        assert!(cfg.read_rgba(&mut mem).is_none());
    }

    #[test]
    fn scanout_read_rgba_is_capped() {
        let mut mem = VecMemory::new(0x1000);

        let pixel_count = MAX_HOST_SCANOUT_RGBA8888_BYTES / 4 + 1;
        let height = u32::try_from(pixel_count).expect("pixel_count fits u32");
        let cfg = AeroGpuScanoutConfig {
            enable: true,
            width: 1,
            height,
            pitch_bytes: 4,
            fb_gpa: 0x100,
            format: AeroGpuFormat::R8G8B8A8Unorm,
        };

        assert!(cfg.read_rgba(&mut mem).is_none());
    }

    #[test]
    fn cursor_read_rgba_is_capped() {
        let mut mem = VecMemory::new(0x1000);

        // 1024x1024 at 4Bpp is exactly 4MiB; exceed it by one row.
        let cfg = AeroGpuCursorConfig {
            enable: true,
            width: 1024,
            height: 1025,
            pitch_bytes: 1024 * 4,
            fb_gpa: 0x100,
            format: AeroGpuFormat::R8G8B8A8Unorm,
            ..Default::default()
        };

        assert!(cfg.read_rgba(&mut mem).is_none());
    }

    #[test]
    fn compositor_honors_hotspot_and_clipping() {
        // 2x2 black scanout.
        let mut scanout = vec![0u8; 2 * 2 * 4];
        for px in scanout.chunks_exact_mut(4) {
            px[3] = 0xff;
        }

        // 2x2 cursor with unique colors per pixel.
        // Layout (top-left origin):
        //   [ red, green ]
        //   [ blue, white ]
        let cursor = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];

        let cfg = AeroGpuCursorConfig {
            enable: true,
            x: 0,
            y: 0,
            hot_x: 1,
            hot_y: 1,
            width: 2,
            height: 2,
            format: AeroGpuFormat::R8G8B8A8Unorm,
            fb_gpa: 0,
            pitch_bytes: 0,
        };

        // With hotspot (1,1) positioned at (0,0), only cursor pixel (1,1) should land on-screen at (0,0).
        composite_cursor_rgba_over_scanout(&mut scanout, 2, 2, &cfg, &cursor).unwrap();
        assert_eq!(&scanout[0..4], &[255, 255, 255, 255]);
        // Everything else remains black.
        assert_eq!(&scanout[4..8], &[0, 0, 0, 255]);
        assert_eq!(&scanout[8..12], &[0, 0, 0, 255]);
        assert_eq!(&scanout[12..16], &[0, 0, 0, 255]);
    }

    #[test]
    fn compositor_alpha_blends_cursor_over_scanout() {
        // 1x1 blue background.
        let mut scanout = vec![0u8, 0, 255, 255];
        // 1x1 red cursor at 50% alpha.
        let cursor = vec![255u8, 0, 0, 128];

        let cfg = AeroGpuCursorConfig {
            enable: true,
            x: 0,
            y: 0,
            hot_x: 0,
            hot_y: 0,
            width: 1,
            height: 1,
            format: AeroGpuFormat::R8G8B8A8Unorm,
            fb_gpa: 0,
            pitch_bytes: 0,
        };

        composite_cursor_rgba_over_scanout(&mut scanout, 1, 1, &cfg, &cursor).unwrap();
        assert_eq!(scanout, vec![128, 0, 127, 255]);
    }

    #[test]
    fn scanout_state_format_mapping_is_conservative_and_deterministic() {
        let cfg = AeroGpuScanoutConfig {
            enable: true,
            width: 640,
            height: 480,
            pitch_bytes: 640 * 4,
            fb_gpa: 0x1234_5678,
            format: AeroGpuFormat::B8G8R8X8Unorm,
        };

        let update = cfg.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.source, SCANOUT_SOURCE_WDDM);
        assert_eq!(update.width, 640);
        assert_eq!(update.height, 480);
        assert_eq!(update.pitch_bytes, 640 * 4);
        assert_eq!(update.base_paddr_lo, 0x1234_5678);
        assert_eq!(update.base_paddr_hi, 0);
        assert_eq!(update.format, SCANOUT_FORMAT_B8G8R8X8);

        // BGRA should preserve the scanout format discriminant.
        let bgra = AeroGpuScanoutConfig {
            format: AeroGpuFormat::B8G8R8A8Unorm,
            ..cfg
        };
        let update = bgra.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.width, 640);
        assert_eq!(update.height, 480);
        assert_eq!(update.pitch_bytes, 640 * 4);
        assert_eq!(update.format, SCANOUT_FORMAT_B8G8R8A8);

        // RGBX/RGBA should be passed through so scanout consumers can swizzle appropriately.
        let rgbx = AeroGpuScanoutConfig {
            format: AeroGpuFormat::R8G8B8X8Unorm,
            ..cfg
        };
        let update = rgbx.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.format, AeroGpuFormat::R8G8B8X8Unorm as u32);

        let rgba = AeroGpuScanoutConfig {
            format: AeroGpuFormat::R8G8B8A8Unorm,
            ..cfg
        };
        let update = rgba.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.format, AeroGpuFormat::R8G8B8A8Unorm as u32);

        // sRGB discriminants should be preserved.
        let bgrx_srgb = AeroGpuScanoutConfig {
            format: AeroGpuFormat::B8G8R8X8UnormSrgb,
            ..cfg
        };
        let update = bgrx_srgb.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.format, SCANOUT_FORMAT_B8G8R8X8_SRGB);

        let bgra_srgb = AeroGpuScanoutConfig {
            format: AeroGpuFormat::B8G8R8A8UnormSrgb,
            ..cfg
        };
        let update = bgra_srgb.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.format, SCANOUT_FORMAT_B8G8R8A8_SRGB);

        let rgbx_srgb = AeroGpuScanoutConfig {
            format: AeroGpuFormat::R8G8B8X8UnormSrgb,
            ..cfg
        };
        let update = rgbx_srgb.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.format, AeroGpuFormat::R8G8B8X8UnormSrgb as u32);

        let rgba_srgb = AeroGpuScanoutConfig {
            format: AeroGpuFormat::R8G8B8A8UnormSrgb,
            ..cfg
        };
        let update = rgba_srgb.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.format, AeroGpuFormat::R8G8B8A8UnormSrgb as u32);

        // 16bpp formats should also be representable in the shared scanout descriptor.
        let b5g6r5 = AeroGpuScanoutConfig {
            pitch_bytes: 640 * 2,
            format: AeroGpuFormat::B5G6R5Unorm,
            ..cfg
        };
        let update = b5g6r5.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.width, 640);
        assert_eq!(update.height, 480);
        assert_eq!(update.pitch_bytes, 640 * 2);
        assert_eq!(update.format, SCANOUT_FORMAT_B5G6R5);

        let b5g5r5a1 = AeroGpuScanoutConfig {
            pitch_bytes: 640 * 2,
            format: AeroGpuFormat::B5G5R5A1Unorm,
            ..cfg
        };
        let update = b5g5r5a1.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.width, 640);
        assert_eq!(update.height, 480);
        assert_eq!(update.pitch_bytes, 640 * 2);
        assert_eq!(update.format, SCANOUT_FORMAT_B5G5R5A1);

        // Unsupported format must not panic and must publish a disabled descriptor.
        let unsupported = AeroGpuScanoutConfig {
            format: AeroGpuFormat::D24UnormS8Uint,
            ..cfg
        };
        let update0 = unsupported.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        let update1 = unsupported.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(
            update0, update1,
            "disabled descriptor must be deterministic"
        );
        assert_eq!(update0.source, SCANOUT_SOURCE_WDDM);
        assert_eq!(update0.width, 0);
        assert_eq!(update0.height, 0);
        assert_eq!(update0.pitch_bytes, 0);
        assert_eq!(update0.base_paddr_lo, 0);
        assert_eq!(update0.base_paddr_hi, 0);
        assert_eq!(update0.format, SCANOUT_FORMAT_B8G8R8X8);

        // Pitch that isn't a multiple of the pixel size is rejected as an invalid descriptor.
        let misaligned_pitch = AeroGpuScanoutConfig {
            pitch_bytes: cfg.pitch_bytes + 2,
            ..cfg
        };
        let update = misaligned_pitch.to_scanout_state_update(SCANOUT_SOURCE_WDDM);
        assert_eq!(update.width, 0);
        assert_eq!(update.height, 0);
    }
}
