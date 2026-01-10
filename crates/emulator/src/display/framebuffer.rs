use core::mem;
use core::sync::atomic::{AtomicU32, Ordering};

pub const FRAMEBUFFER_MAGIC: u32 = 0x4f52_4541; // "AERO" in little-endian u32
pub const FRAMEBUFFER_VERSION: u32 = 1;

pub const FRAMEBUFFER_FORMAT_RGBA8888: u32 = 1;

pub const HEADER_INDEX_MAGIC: usize = 0;
pub const HEADER_INDEX_VERSION: usize = 1;
pub const HEADER_INDEX_WIDTH: usize = 2;
pub const HEADER_INDEX_HEIGHT: usize = 3;
pub const HEADER_INDEX_STRIDE_BYTES: usize = 4;
pub const HEADER_INDEX_FORMAT: usize = 5;
pub const HEADER_INDEX_FRAME_COUNTER: usize = 6;
pub const HEADER_INDEX_CONFIG_COUNTER: usize = 7;

pub const HEADER_I32_COUNT: usize = 8;
pub const HEADER_BYTE_LENGTH: usize = HEADER_I32_COUNT * mem::size_of::<u32>();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramebufferError {
    BufferTooSmall,
    Misaligned,
    InvalidDimensions,
    UnsupportedFormat,
}

#[repr(C)]
pub struct FramebufferHeader {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub stride_bytes: AtomicU32,
    pub format: AtomicU32,
    pub frame_counter: AtomicU32,
    pub config_counter: AtomicU32,
}

impl FramebufferHeader {
    pub const fn new() -> Self {
        Self {
            magic: AtomicU32::new(FRAMEBUFFER_MAGIC),
            version: AtomicU32::new(FRAMEBUFFER_VERSION),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            stride_bytes: AtomicU32::new(0),
            format: AtomicU32::new(FRAMEBUFFER_FORMAT_RGBA8888),
            frame_counter: AtomicU32::new(0),
            config_counter: AtomicU32::new(0),
        }
    }
}

pub fn required_framebuffer_bytes(width: u32, height: u32, stride_bytes: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    if stride_bytes < width.checked_mul(4)? {
        return None;
    }
    let pixels = usize::try_from(stride_bytes).ok()?.checked_mul(usize::try_from(height).ok()?)?;
    HEADER_BYTE_LENGTH.checked_add(pixels)
}

pub struct SharedFramebuffer<'a> {
    header: &'a FramebufferHeader,
    pixels: &'a mut [u8],
}

impl<'a> SharedFramebuffer<'a> {
    pub fn from_bytes(bytes: &'a mut [u8]) -> Result<Self, FramebufferError> {
        if bytes.len() < HEADER_BYTE_LENGTH {
            return Err(FramebufferError::BufferTooSmall);
        }
        if (bytes.as_ptr() as usize) % mem::align_of::<FramebufferHeader>() != 0 {
            return Err(FramebufferError::Misaligned);
        }

        let (header_bytes, pixels) = bytes.split_at_mut(HEADER_BYTE_LENGTH);
        let header_ptr = header_bytes.as_ptr() as *const FramebufferHeader;
        let header = unsafe { &*header_ptr };

        Ok(Self { header, pixels })
    }

    pub fn header(&self) -> &FramebufferHeader {
        self.header
    }

    pub fn pixels_capacity_bytes(&self) -> usize {
        self.pixels.len()
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    pub fn initialize_rgba8888(&self) {
        self.header.magic.store(FRAMEBUFFER_MAGIC, Ordering::Relaxed);
        self.header.version.store(FRAMEBUFFER_VERSION, Ordering::Relaxed);
        self.header.format.store(FRAMEBUFFER_FORMAT_RGBA8888, Ordering::Relaxed);
        self.header.frame_counter.store(0, Ordering::Relaxed);
        self.header.config_counter.store(0, Ordering::Relaxed);
    }

    pub fn set_mode(&self, width: u32, height: u32, stride_bytes: u32) -> Result<(), FramebufferError> {
        if width == 0 || height == 0 {
            return Err(FramebufferError::InvalidDimensions);
        }
        let min_stride = width.checked_mul(4).ok_or(FramebufferError::InvalidDimensions)?;
        if stride_bytes < min_stride || stride_bytes % 4 != 0 {
            return Err(FramebufferError::InvalidDimensions);
        }
        let required = required_framebuffer_bytes(width, height, stride_bytes).ok_or(FramebufferError::InvalidDimensions)?;
        if required - HEADER_BYTE_LENGTH > self.pixels_capacity_bytes() {
            return Err(FramebufferError::BufferTooSmall);
        }

        let prev_width = self.header.width.load(Ordering::Relaxed);
        let prev_height = self.header.height.load(Ordering::Relaxed);
        let prev_stride = self.header.stride_bytes.load(Ordering::Relaxed);
        if prev_width == width && prev_height == height && prev_stride == stride_bytes {
            return Ok(());
        }

        self.header.width.store(width, Ordering::Relaxed);
        self.header.height.store(height, Ordering::Relaxed);
        self.header.stride_bytes.store(stride_bytes, Ordering::Relaxed);

        self.header.format.store(FRAMEBUFFER_FORMAT_RGBA8888, Ordering::Relaxed);
        self.header
            .config_counter
            .fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub fn present_rgba8888(&mut self, width: u32, height: u32, stride_bytes: u32, pixels: &[u8]) -> Result<(), FramebufferError> {
        if self.header.format.load(Ordering::Relaxed) != FRAMEBUFFER_FORMAT_RGBA8888 {
            return Err(FramebufferError::UnsupportedFormat);
        }

        self.set_mode(width, height, stride_bytes)?;

        let required_pixels = usize::try_from(stride_bytes)
            .map_err(|_| FramebufferError::InvalidDimensions)?
            .checked_mul(usize::try_from(height).map_err(|_| FramebufferError::InvalidDimensions)?)
            .ok_or(FramebufferError::InvalidDimensions)?;

        if required_pixels > self.pixels.len() || pixels.len() < required_pixels {
            return Err(FramebufferError::BufferTooSmall);
        }

        self.pixels[..required_pixels].copy_from_slice(&pixels[..required_pixels]);

        // Ensure pixel writes are visible before the counter update.
        self.header.frame_counter.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Presents a tightly-packed BGRA8888 pixel buffer as RGBA8888 into the shared framebuffer.
    ///
    /// This is useful for VBE 32bpp modes which conventionally expose pixels as little-endian
    /// `0xAARRGGBB` (byte order BGRA). The browser-facing shared framebuffer is always RGBA.
    ///
    /// Note: the top byte in VBE 32bpp modes is typically documented as "reserved" and is often
    /// left as 0 by guest software. Since the shared framebuffer feeds `ImageData`, which treats
    /// alpha=0 as fully transparent, we force the output alpha to 0xFF (opaque).
    pub fn present_bgra8888_u32(&mut self, width: u32, height: u32, pixels: &[u32]) -> Result<(), FramebufferError> {
        if self.header.format.load(Ordering::Relaxed) != FRAMEBUFFER_FORMAT_RGBA8888 {
            return Err(FramebufferError::UnsupportedFormat);
        }

        let stride_bytes = width
            .checked_mul(4)
            .ok_or(FramebufferError::InvalidDimensions)?;
        self.set_mode(width, height, stride_bytes)?;

        let expected = usize::try_from(width)
            .map_err(|_| FramebufferError::InvalidDimensions)?
            .checked_mul(usize::try_from(height).map_err(|_| FramebufferError::InvalidDimensions)?)
            .ok_or(FramebufferError::InvalidDimensions)?;

        if pixels.len() < expected {
            return Err(FramebufferError::BufferTooSmall);
        }

        let required_bytes = expected
            .checked_mul(4)
            .ok_or(FramebufferError::InvalidDimensions)?;
        if required_bytes > self.pixels.len() {
            return Err(FramebufferError::BufferTooSmall);
        }

        let dst =
            unsafe { core::slice::from_raw_parts_mut(self.pixels.as_mut_ptr() as *mut u32, expected) };
        for (d, &src) in dst.iter_mut().zip(&pixels[..expected]) {
            // Convert BGRA -> RGBA (swap R/B bytes).
            *d = (src & 0x0000FF00)
                | ((src & 0x00FF0000) >> 16)
                | ((src & 0x000000FF) << 16)
                | 0xFF00_0000;
        }

        self.header.frame_counter.fetch_add(1, Ordering::Release);
        Ok(())
    }
}

pub struct OwnedSharedFramebuffer {
    words: Box<[u32]>,
}

impl OwnedSharedFramebuffer {
    pub fn new(width: u32, height: u32, stride_bytes: u32) -> Result<Self, FramebufferError> {
        let total_bytes = required_framebuffer_bytes(width, height, stride_bytes).ok_or(FramebufferError::InvalidDimensions)?;
        if total_bytes % 4 != 0 {
            return Err(FramebufferError::InvalidDimensions);
        }
        let words_len = total_bytes / 4;

        let mut words = vec![0u32; words_len].into_boxed_slice();

        let header_ptr = words.as_mut_ptr() as *mut FramebufferHeader;
        unsafe {
            header_ptr.write(FramebufferHeader::new());
            let header = &*header_ptr;
            header.width.store(width, Ordering::Relaxed);
            header.height.store(height, Ordering::Relaxed);
            header.stride_bytes.store(stride_bytes, Ordering::Relaxed);
            header.format.store(FRAMEBUFFER_FORMAT_RGBA8888, Ordering::Relaxed);
            header.config_counter.store(1, Ordering::Relaxed);
        }

        Ok(Self { words })
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.words.as_ptr() as *const u8, self.words.len() * 4) }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.words.as_mut_ptr() as *mut u8, self.words.len() * 4) }
    }

    pub fn ptr(&self) -> *const u8 {
        self.as_bytes().as_ptr()
    }

    pub fn len_bytes(&self) -> usize {
        self.as_bytes().len()
    }

    pub fn view_mut(&mut self) -> SharedFramebuffer<'_> {
        SharedFramebuffer::from_bytes(self.as_bytes_mut()).expect("owned framebuffer is always aligned and large enough")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_is_stable() {
        assert_eq!(mem::size_of::<FramebufferHeader>(), HEADER_BYTE_LENGTH);
        assert_eq!(mem::align_of::<FramebufferHeader>(), 4);

        let header = FramebufferHeader::new();
        let base = &header as *const _ as usize;

        let offset = |field: *const AtomicU32| (field as usize) - base;

        assert_eq!(offset(&header.magic), HEADER_INDEX_MAGIC * 4);
        assert_eq!(offset(&header.version), HEADER_INDEX_VERSION * 4);
        assert_eq!(offset(&header.width), HEADER_INDEX_WIDTH * 4);
        assert_eq!(offset(&header.height), HEADER_INDEX_HEIGHT * 4);
        assert_eq!(offset(&header.stride_bytes), HEADER_INDEX_STRIDE_BYTES * 4);
        assert_eq!(offset(&header.format), HEADER_INDEX_FORMAT * 4);
        assert_eq!(offset(&header.frame_counter), HEADER_INDEX_FRAME_COUNTER * 4);
        assert_eq!(offset(&header.config_counter), HEADER_INDEX_CONFIG_COUNTER * 4);
    }

    #[test]
    fn owned_framebuffer_allocates_expected_size() {
        let fb = OwnedSharedFramebuffer::new(320, 200, 320 * 4).unwrap();
        assert_eq!(fb.len_bytes(), HEADER_BYTE_LENGTH + (320 * 4 * 200) as usize);
    }
}
