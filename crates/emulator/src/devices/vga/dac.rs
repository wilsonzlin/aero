#[derive(Debug, Clone)]
pub struct VgaDac {
    palette_rgb6: [[u8; 3]; 256],
    palette_rgba: [u32; 256],
    pel_mask: u8,
    dirty: bool,

    write_index: u8,
    read_index: u8,
    component_index: u8,
}

impl VgaDac {
    pub fn new() -> Self {
        Self {
            palette_rgb6: [[0u8; 3]; 256],
            palette_rgba: [0u32; 256],
            pel_mask: 0xFF,
            dirty: true,
            write_index: 0,
            read_index: 0,
            component_index: 0,
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

        self.palette_rgb6[index as usize] = [r, g, b];
        let rgba = pack_rgba(
            scale_6bit_to_8bit(r),
            scale_6bit_to_8bit(g),
            scale_6bit_to_8bit(b),
            0xFF,
        );
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

    #[inline]
    pub fn palette_rgb6(&self) -> &[[u8; 3]; 256] {
        &self.palette_rgb6
    }

    /// Returns whether the palette or PEL mask changed since the last render.
    #[inline]
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }

    /// Write to one of the VGA DAC ports (0x3C6..=0x3C9).
    pub fn port_write(&mut self, port: u16, value: u8) {
        match port {
            0x3C6 => self.set_pel_mask(value),
            0x3C7 => {
                self.read_index = value;
                self.component_index = 0;
            }
            0x3C8 => {
                self.write_index = value;
                self.component_index = 0;
            }
            0x3C9 => self.write_data(value),
            _ => {}
        }
    }

    /// Read from one of the VGA DAC ports (0x3C6 or 0x3C9).
    pub fn port_read(&mut self, port: u16) -> u8 {
        match port {
            0x3C6 => self.pel_mask,
            0x3C9 => self.read_data(),
            _ => 0xFF,
        }
    }

    fn write_data(&mut self, value: u8) {
        let idx = self.write_index as usize;
        let component = self.component_index as usize;
        self.palette_rgb6[idx][component] = value & 0x3F;
        self.component_index += 1;
        if self.component_index == 3 {
            self.component_index = 0;
            let [r, g, b] = self.palette_rgb6[idx];
            let rgba = pack_rgba(
                scale_6bit_to_8bit(r),
                scale_6bit_to_8bit(g),
                scale_6bit_to_8bit(b),
                0xFF,
            );
            self.palette_rgba[idx] = rgba;
            self.dirty = true;
            self.write_index = self.write_index.wrapping_add(1);
        }
    }

    fn read_data(&mut self) -> u8 {
        let idx = self.read_index as usize;
        let component = self.component_index as usize;
        let out = self.palette_rgb6[idx][component];
        self.component_index += 1;
        if self.component_index == 3 {
            self.component_index = 0;
            self.read_index = self.read_index.wrapping_add(1);
        }
        out
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
