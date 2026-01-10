#[derive(Debug, Clone)]
pub struct VgaDac {
    palette_rgba: [u32; 256],
    pel_mask: u8,
    dirty: bool,
}

impl VgaDac {
    pub fn new() -> Self {
        Self {
            palette_rgba: [0u32; 256],
            pel_mask: 0xFF,
            dirty: true,
        }
    }

    #[inline]
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Sets a DAC entry using VGA-style 6-bit channels (0..=63).
    pub fn set_entry_6bit(&mut self, index: u8, r: u8, g: u8, b: u8) {
        debug_assert!(r <= 63, "red channel must be 6-bit (0..=63)");
        debug_assert!(g <= 63, "green channel must be 6-bit (0..=63)");
        debug_assert!(b <= 63, "blue channel must be 6-bit (0..=63)");

        let rgba = pack_rgba(scale_6bit_to_8bit(r), scale_6bit_to_8bit(g), scale_6bit_to_8bit(b), 0xFF);
        self.palette_rgba[index as usize] = rgba;
        self.dirty = true;
    }

    pub fn set_pel_mask(&mut self, mask: u8) {
        if self.pel_mask == mask {
            return;
        }
        self.pel_mask = mask;
        self.dirty = true;
    }

    #[inline]
    pub fn pel_mask(&self) -> u8 {
        self.pel_mask
    }

    #[inline]
    pub fn palette_rgba(&self) -> &[u32; 256] {
        &self.palette_rgba
    }

    /// Returns whether the palette or PEL mask changed since the last render.
    #[inline]
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }
}

#[inline]
fn scale_6bit_to_8bit(v: u8) -> u8 {
    // Linear expansion from 0..=63 to 0..=255.
    ((u16::from(v) * 255) / 63) as u8
}

#[inline]
pub fn pack_rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    u32::from_le_bytes([r, g, b, a])
}

