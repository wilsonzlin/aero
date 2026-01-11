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
        } else {
            Self::Invalid
        }
    }

    pub fn bytes_per_pixel(self) -> Option<usize> {
        match self {
            Self::Invalid | Self::D24UnormS8Uint | Self::D32Float => None,
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
                | AeroGpuFormat::D32Float => return None,
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
