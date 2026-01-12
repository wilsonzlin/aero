use memory::MemoryBus;

use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat as ProtocolAerogpuFormat;

// Values derived from the canonical `aero-protocol` definition of `enum aerogpu_format`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AeroGpuFormat {
    Invalid = ProtocolAerogpuFormat::Invalid as u32,
    B8G8R8A8Unorm = ProtocolAerogpuFormat::B8G8R8A8Unorm as u32,
    B8G8R8X8Unorm = ProtocolAerogpuFormat::B8G8R8X8Unorm as u32,
    R8G8B8A8Unorm = ProtocolAerogpuFormat::R8G8B8A8Unorm as u32,
    R8G8B8X8Unorm = ProtocolAerogpuFormat::R8G8B8X8Unorm as u32,
    B5G6R5Unorm = ProtocolAerogpuFormat::B5G6R5Unorm as u32,
    B5G5R5A1Unorm = ProtocolAerogpuFormat::B5G5R5A1Unorm as u32,
    D24UnormS8Uint = ProtocolAerogpuFormat::D24UnormS8Uint as u32,
    D32Float = ProtocolAerogpuFormat::D32Float as u32,
    // Values reserved for future protocol extensions. The scanout/cursor paths do not currently
    // support these formats, but we keep them representable so the software executor can size and
    // safely ignore them.
    Bc1Unorm = 64,
    Bc2Unorm = 65,
    Bc3Unorm = 66,
    Bc7Unorm = 67,
}

impl AeroGpuFormat {
    pub fn from_u32(value: u32) -> Self {
        if value == Self::B8G8R8A8Unorm as u32 {
            Self::B8G8R8A8Unorm
        } else if value == Self::B8G8R8X8Unorm as u32 {
            Self::B8G8R8X8Unorm
        } else if value == Self::R8G8B8A8Unorm as u32 {
            Self::R8G8B8A8Unorm
        } else if value == Self::R8G8B8X8Unorm as u32 {
            Self::R8G8B8X8Unorm
        } else if value == Self::B5G6R5Unorm as u32 {
            Self::B5G6R5Unorm
        } else if value == Self::B5G5R5A1Unorm as u32 {
            Self::B5G5R5A1Unorm
        } else if value == Self::D24UnormS8Uint as u32 {
            Self::D24UnormS8Uint
        } else if value == Self::D32Float as u32 {
            Self::D32Float
        } else if value == Self::Bc1Unorm as u32 {
            Self::Bc1Unorm
        } else if value == Self::Bc2Unorm as u32 {
            Self::Bc2Unorm
        } else if value == Self::Bc3Unorm as u32 {
            Self::Bc3Unorm
        } else if value == Self::Bc7Unorm as u32 {
            Self::Bc7Unorm
        } else {
            Self::Invalid
        }
    }

    pub fn bytes_per_pixel(self) -> Option<usize> {
        match self {
            Self::Invalid
            | Self::D24UnormS8Uint
            | Self::D32Float
            | Self::Bc1Unorm
            | Self::Bc2Unorm
            | Self::Bc3Unorm
            | Self::Bc7Unorm => None,
            Self::B8G8R8A8Unorm
            | Self::B8G8R8X8Unorm
            | Self::R8G8B8A8Unorm
            | Self::R8G8B8X8Unorm => Some(4),
            Self::B5G6R5Unorm | Self::B5G5R5A1Unorm => Some(2),
        }
    }
}

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

        let mut out = vec![0u8; width * height * 4];
        let mut row_buf = vec![0u8; row_bytes];

        for y in 0..height {
            let row_gpa = self.fb_gpa + (y as u64) * (self.pitch_bytes as u64);
            mem.read_physical(row_gpa, &mut row_buf);
            let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];

            match self.format {
                AeroGpuFormat::B8G8R8A8Unorm => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = src[3];
                    }
                }
                AeroGpuFormat::B8G8R8X8Unorm => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = 0xff;
                    }
                }
                AeroGpuFormat::R8G8B8A8Unorm => {
                    dst_row.copy_from_slice(&row_buf);
                }
                AeroGpuFormat::R8G8B8X8Unorm => {
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
                AeroGpuFormat::Invalid
                | AeroGpuFormat::D24UnormS8Uint
                | AeroGpuFormat::D32Float
                | AeroGpuFormat::Bc1Unorm
                | AeroGpuFormat::Bc2Unorm
                | AeroGpuFormat::Bc3Unorm
                | AeroGpuFormat::Bc7Unorm => return None,
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

        let mut out = vec![0u8; width * height * 4];
        let mut row_buf = vec![0u8; row_bytes];

        for y in 0..height {
            let row_gpa = self.fb_gpa + (y as u64) * (self.pitch_bytes as u64);
            mem.read_physical(row_gpa, &mut row_buf);
            let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];

            match self.format {
                AeroGpuFormat::B8G8R8A8Unorm => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = src[3];
                    }
                }
                AeroGpuFormat::R8G8B8A8Unorm => {
                    dst_row.copy_from_slice(&row_buf);
                }
                // Cursor should be ARGB, but accept XRGB for now (opaque alpha) so
                // diagnostics/debug cursors are visible even if the guest picks X8R8G8B8.
                AeroGpuFormat::B8G8R8X8Unorm => {
                    for x in 0..width {
                        let src = &row_buf[x * 4..x * 4 + 4];
                        let dst = &mut dst_row[x * 4..x * 4 + 4];
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                        dst[3] = 0xff;
                    }
                }
                AeroGpuFormat::R8G8B8X8Unorm => {
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
}
