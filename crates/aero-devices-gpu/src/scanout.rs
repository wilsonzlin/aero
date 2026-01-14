use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat as ProtocolAerogpuFormat;
use memory::MemoryBus;

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
    B8G8R8A8UnormSrgb = ProtocolAerogpuFormat::B8G8R8A8UnormSrgb as u32,
    B8G8R8X8UnormSrgb = ProtocolAerogpuFormat::B8G8R8X8UnormSrgb as u32,
    R8G8B8A8UnormSrgb = ProtocolAerogpuFormat::R8G8B8A8UnormSrgb as u32,
    R8G8B8X8UnormSrgb = ProtocolAerogpuFormat::R8G8B8X8UnormSrgb as u32,
    D24UnormS8Uint = ProtocolAerogpuFormat::D24UnormS8Uint as u32,
    D32Float = ProtocolAerogpuFormat::D32Float as u32,
    // The scanout/cursor paths do not currently support BC formats, but we keep them representable
    // so higher layers can compute backing sizes (and ignore them when presenting).
    Bc1Unorm = ProtocolAerogpuFormat::BC1RgbaUnorm as u32,
    Bc1UnormSrgb = ProtocolAerogpuFormat::BC1RgbaUnormSrgb as u32,
    Bc2Unorm = ProtocolAerogpuFormat::BC2RgbaUnorm as u32,
    Bc2UnormSrgb = ProtocolAerogpuFormat::BC2RgbaUnormSrgb as u32,
    Bc3Unorm = ProtocolAerogpuFormat::BC3RgbaUnorm as u32,
    Bc3UnormSrgb = ProtocolAerogpuFormat::BC3RgbaUnormSrgb as u32,
    Bc7Unorm = ProtocolAerogpuFormat::BC7RgbaUnorm as u32,
    Bc7UnormSrgb = ProtocolAerogpuFormat::BC7RgbaUnormSrgb as u32,
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
        } else if value == Self::B8G8R8A8UnormSrgb as u32 {
            Self::B8G8R8A8UnormSrgb
        } else if value == Self::B8G8R8X8UnormSrgb as u32 {
            Self::B8G8R8X8UnormSrgb
        } else if value == Self::R8G8B8A8UnormSrgb as u32 {
            Self::R8G8B8A8UnormSrgb
        } else if value == Self::R8G8B8X8UnormSrgb as u32 {
            Self::R8G8B8X8UnormSrgb
        } else if value == Self::D24UnormS8Uint as u32 {
            Self::D24UnormS8Uint
        } else if value == Self::D32Float as u32 {
            Self::D32Float
        } else if value == Self::Bc1Unorm as u32 {
            Self::Bc1Unorm
        } else if value == Self::Bc1UnormSrgb as u32 {
            Self::Bc1UnormSrgb
        } else if value == Self::Bc2Unorm as u32 {
            Self::Bc2Unorm
        } else if value == Self::Bc2UnormSrgb as u32 {
            Self::Bc2UnormSrgb
        } else if value == Self::Bc3Unorm as u32 {
            Self::Bc3Unorm
        } else if value == Self::Bc3UnormSrgb as u32 {
            Self::Bc3UnormSrgb
        } else if value == Self::Bc7Unorm as u32 {
            Self::Bc7Unorm
        } else if value == Self::Bc7UnormSrgb as u32 {
            Self::Bc7UnormSrgb
        } else {
            Self::Invalid
        }
    }

    pub fn bytes_per_pixel(self) -> Option<usize> {
        match self {
            Self::B8G8R8A8Unorm
            | Self::B8G8R8X8Unorm
            | Self::R8G8B8A8Unorm
            | Self::R8G8B8X8Unorm
            | Self::B8G8R8A8UnormSrgb
            | Self::B8G8R8X8UnormSrgb
            | Self::R8G8B8A8UnormSrgb
            | Self::R8G8B8X8UnormSrgb => Some(4),
            Self::B5G6R5Unorm | Self::B5G5R5A1Unorm => Some(2),
            _ => None,
        }
    }
}

fn convert_row_to_rgba(
    format: AeroGpuFormat,
    width: usize,
    src_row: &[u8],
    dst_row: &mut [u8],
) -> Option<()> {
    match format {
        AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8A8UnormSrgb => {
            for x in 0..width {
                let src = &src_row[x * 4..x * 4 + 4];
                let dst = &mut dst_row[x * 4..x * 4 + 4];
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
        }
        AeroGpuFormat::B8G8R8X8Unorm | AeroGpuFormat::B8G8R8X8UnormSrgb => {
            for x in 0..width {
                let src = &src_row[x * 4..x * 4 + 4];
                let dst = &mut dst_row[x * 4..x * 4 + 4];
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = 0xff;
            }
        }
        AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8A8UnormSrgb => {
            dst_row.copy_from_slice(&src_row[..dst_row.len()]);
        }
        AeroGpuFormat::R8G8B8X8Unorm | AeroGpuFormat::R8G8B8X8UnormSrgb => {
            for x in 0..width {
                let src = &src_row[x * 4..x * 4 + 4];
                let dst = &mut dst_row[x * 4..x * 4 + 4];
                dst[0] = src[0];
                dst[1] = src[1];
                dst[2] = src[2];
                dst[3] = 0xff;
            }
        }
        AeroGpuFormat::B5G6R5Unorm => {
            for x in 0..width {
                let pix = u16::from_le_bytes([src_row[x * 2], src_row[x * 2 + 1]]);
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
                let pix = u16::from_le_bytes([src_row[x * 2], src_row[x * 2 + 1]]);
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

    Some(())
}

fn read_rgba_from_guest(
    enable: bool,
    width: u32,
    height: u32,
    format: AeroGpuFormat,
    pitch_bytes: u32,
    fb_gpa: u64,
    mem: &mut dyn MemoryBus,
) -> Option<Vec<u8>> {
    if !enable {
        return None;
    }

    let bytes_per_pixel = format.bytes_per_pixel()?;
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;

    if width == 0 || height == 0 {
        return None;
    }
    if fb_gpa == 0 {
        return None;
    }

    let pitch = usize::try_from(pitch_bytes).ok()?;
    let row_bytes = width.checked_mul(bytes_per_pixel)?;
    if pitch < row_bytes {
        return None;
    }

    // Validate GPA arithmetic does not wrap.
    let pitch_u64 = u64::from(pitch_bytes);
    let row_bytes_u64 = u64::try_from(row_bytes).ok()?;
    let last_row_gpa =
        fb_gpa.checked_add((height as u64).checked_sub(1)?.checked_mul(pitch_u64)?)?;
    last_row_gpa.checked_add(row_bytes_u64)?;

    let out_len = width.checked_mul(height)?.checked_mul(4)?;
    let mut out = vec![0u8; out_len];
    let mut row_buf = vec![0u8; row_bytes];

    for y in 0..height {
        let row_gpa = fb_gpa + (y as u64) * pitch_u64;
        mem.read_physical(row_gpa, &mut row_buf);
        let dst_row = &mut out[y * width * 4..(y + 1) * width * 4];
        convert_row_to_rgba(format, width, &row_buf, dst_row)?;
    }

    Some(out)
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
        read_rgba_from_guest(
            self.enable,
            self.width,
            self.height,
            self.format,
            self.pitch_bytes,
            self.fb_gpa,
            mem,
        )
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

        // MVP: only support 32bpp cursor formats.
        let bytes_per_pixel = self.format.bytes_per_pixel()?;
        if bytes_per_pixel != 4 {
            return None;
        }

        read_rgba_from_guest(
            self.enable,
            self.width,
            self.height,
            self.format,
            self.pitch_bytes,
            self.fb_gpa,
            mem,
        )
    }
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
            Self { data: vec![0; size] }
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
    fn scanout_read_b8g8r8x8_sets_opaque_alpha() {
        let mut mem = VecMemory::new(0x1000);
        let fb_gpa = 0x100u64;

        // 1x1 pixel stored as BGRX.
        mem.write_physical(fb_gpa, &[3, 2, 1, 0x00]);

        let cfg = AeroGpuScanoutConfig {
            enable: true,
            width: 1,
            height: 1,
            pitch_bytes: 4,
            fb_gpa,
            format: AeroGpuFormat::B8G8R8X8Unorm,
        };

        assert_eq!(cfg.read_rgba(&mut mem).unwrap(), vec![1, 2, 3, 0xff]);
    }

    #[test]
    fn scanout_read_respects_pitch_padding() {
        let mut mem = VecMemory::new(0x1000);
        let fb_gpa = 0x200u64;

        // 2x2 pixels (row_bytes=8) but pitch is 12 with 4 bytes padding per row.
        // Row 0: (R=1,G=2,B=3), (R=4,G=5,B=6)
        // Row 1: (R=7,G=8,B=9), (R=10,G=11,B=12)
        mem.write_physical(
            fb_gpa,
            &[
                3, 2, 1, 0, 6, 5, 4, 0, 0xDE, 0xAD, 0xBE, 0xEF, // padding
                9, 8, 7, 0, 12, 11, 10, 0, 0xFE, 0xED, 0xFA, 0xCE, // padding
            ],
        );

        let cfg = AeroGpuScanoutConfig {
            enable: true,
            width: 2,
            height: 2,
            pitch_bytes: 12,
            fb_gpa,
            format: AeroGpuFormat::B8G8R8X8Unorm,
        };

        assert_eq!(
            cfg.read_rgba(&mut mem).unwrap(),
            vec![
                1, 2, 3, 0xff, 4, 5, 6, 0xff, // row 0
                7, 8, 9, 0xff, 10, 11, 12, 0xff // row 1
            ]
        );
    }

    #[test]
    fn read_rgba_rejects_pitch_too_small() {
        let mut mem = VecMemory::new(0x1000);
        let fb_gpa = 0x100u64;

        let cfg = AeroGpuScanoutConfig {
            enable: true,
            width: 2,
            height: 1,
            // Needs at least 8 bytes (2 * 4Bpp).
            pitch_bytes: 4,
            fb_gpa,
            format: AeroGpuFormat::R8G8B8A8Unorm,
        };
        assert!(cfg.read_rgba(&mut mem).is_none());

        let cursor = AeroGpuCursorConfig {
            enable: true,
            width: 2,
            height: 1,
            pitch_bytes: 4,
            fb_gpa,
            format: AeroGpuFormat::R8G8B8A8Unorm,
            ..Default::default()
        };
        assert!(cursor.read_rgba(&mut mem).is_none());
    }

    #[test]
    fn read_rgba_rejects_fb_gpa_zero() {
        let mut mem = VecMemory::new(0x1000);

        let scanout = AeroGpuScanoutConfig {
            enable: true,
            width: 1,
            height: 1,
            pitch_bytes: 4,
            fb_gpa: 0,
            format: AeroGpuFormat::R8G8B8A8Unorm,
        };
        assert!(scanout.read_rgba(&mut mem).is_none());

        let cursor = AeroGpuCursorConfig {
            enable: true,
            width: 1,
            height: 1,
            pitch_bytes: 4,
            fb_gpa: 0,
            format: AeroGpuFormat::R8G8B8A8Unorm,
            ..Default::default()
        };
        assert!(cursor.read_rgba(&mut mem).is_none());
    }
}
