//! VGA/SVGA (VBE) device model.
//!
//! This crate is intentionally self-contained so it can be wired into the rest
//! of the emulator later. It provides:
//! - VGA register file emulation (sequencer/graphics/attribute/CRTC) with the
//!   subset of behavior needed for BIOS + early boot.
//! - Text mode (80x25) rendering with a built-in bitmap font and cursor.
//! - Mode 13h (320x200x256) rendering (chain-4).
//! - A Bochs-compatible VBE ("VBE_DISPI") interface for linear framebuffer
//!   modes commonly used by boot loaders/Windows boot splash.
//! - VRAM access helpers for mapping the legacy regions (0xA0000 etc) and the
//!   SVGA linear framebuffer base (0xE0000000 by default).
//!
//! The `u32` framebuffer format is RGBA8888 in native-endian `u32`, where the
//! least significant byte is **R** (i.e. `0xAABBGGRR` on big-endian, but the
//! byte order in memory on little-endian is `[R, G, B, A]`, matching Canvas
//! `ImageData`).

mod palette;
mod snapshot;
mod text_font;

use palette::{rgb_to_rgba_u32, Rgb};
pub use snapshot::{VgaSnapshotError, VgaSnapshotV1};
use text_font::FONT8X8_BASIC;

/// Physical base address for the Bochs VBE linear framebuffer (LFB).
///
/// QEMU/Bochs typically map the LFB at 0xE0000000.
pub const SVGA_LFB_BASE: u32 = 0xE000_0000;

/// Size of VGA plane memory (64KiB).
pub const VGA_PLANE_SIZE: usize = 64 * 1024;

/// Total VGA memory for 4 planes (256KiB).
pub const VGA_VRAM_SIZE: usize = 4 * VGA_PLANE_SIZE;

/// Default total VRAM for the device (16MiB), enough for common VBE modes.
pub const DEFAULT_VRAM_SIZE: usize = 16 * 1024 * 1024;

/// Host-facing display trait (to be shared with the rest of the emulator).
pub trait DisplayOutput {
    /// Returns the current visible framebuffer (front buffer) as RGBA8888.
    fn get_framebuffer(&self) -> &[u32];

    /// Returns the current output resolution.
    fn get_resolution(&self) -> (u32, u32);

    /// Re-renders into the front buffer if necessary.
    fn present(&mut self);
}

/// Port I/O trait (to be shared with the CPU/machine).
pub trait PortIO {
    fn port_read(&mut self, port: u16, size: usize) -> u32;
    fn port_write(&mut self, port: u16, size: usize, val: u32);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Text80x25,
    Mode13h,
    Planar4bpp { width: u32, height: u32 },
    SvgaLinear { width: u32, height: u32, bpp: u16 },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct VbeRegs {
    pub xres: u16,
    pub yres: u16,
    pub bpp: u16,
    pub enable: u16,
    pub bank: u16,
    pub virt_width: u16,
    pub virt_height: u16,
    pub x_offset: u16,
    pub y_offset: u16,
}

impl VbeRegs {
    fn enabled(self) -> bool {
        (self.enable & 0x0001) != 0
    }

    fn lfb_enabled(self) -> bool {
        (self.enable & 0x0040) != 0
    }

    fn effective_stride_pixels(self) -> u32 {
        let vw = self.virt_width as u32;
        if vw != 0 {
            vw
        } else {
            self.xres as u32
        }
    }
}

/// VGA/SVGA device.
pub struct VgaDevice {
    // Core VGA registers.
    misc_output: u8,

    sequencer_index: u8,
    sequencer: [u8; 5],

    graphics_index: u8,
    graphics: [u8; 9],

    crtc_index: u8,
    crtc: [u8; 25],

    attribute_index: u8,
    attribute_flip_flop_data: bool,
    attribute: [u8; 21],
    input_status1_vretrace: bool,

    // DAC / palette.
    pel_mask: u8,
    dac_write_index: u8,
    dac_write_subindex: u8,
    dac_read_index: u8,
    dac_read_subindex: u8,
    dac: [Rgb; 256],

    // Bochs VBE.
    vbe_index: u16,
    pub vbe: VbeRegs,

    // VRAM: the first 256KiB are treated as planar VGA memory (4 planes).
    // SVGA linear modes use the same underlying VRAM starting at offset 0.
    vram: Vec<u8>,
    latches: [u8; 4],

    // Output buffers.
    front: Vec<u32>,
    back: Vec<u32>,
    width: u32,
    height: u32,
    dirty: bool,
}

impl Default for VgaDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl VgaDevice {
    pub fn new() -> Self {
        let mut device = Self {
            misc_output: 0,
            sequencer_index: 0,
            sequencer: [0; 5],
            graphics_index: 0,
            graphics: [0; 9],
            crtc_index: 0,
            crtc: [0; 25],
            attribute_index: 0,
            attribute_flip_flop_data: false,
            attribute: [0; 21],
            input_status1_vretrace: false,
            pel_mask: 0xFF,
            dac_write_index: 0,
            dac_write_subindex: 0,
            dac_read_index: 0,
            dac_read_subindex: 0,
            dac: [Rgb::BLACK; 256],
            vbe_index: 0,
            vbe: VbeRegs::default(),
            vram: vec![0; DEFAULT_VRAM_SIZE],
            latches: [0; 4],
            front: Vec::new(),
            back: Vec::new(),
            width: 0,
            height: 0,
            dirty: true,
        };

        device.reset_palette();
        device.set_text_mode_80x25();
        device.present();
        device
    }

    /// Resets the DAC to a sensible default VGA palette (EGA 16-color + 256-color cube).
    pub fn reset_palette(&mut self) {
        self.dac = palette::default_vga_palette();
        self.pel_mask = 0xFF;
    }

    /// Convenience helper: configure the register file for VGA text mode 80x25.
    pub fn set_text_mode_80x25(&mut self) {
        // Attribute mode control: bit0=0 => text.
        self.attribute[0x10] = 1 << 2; // line graphics enable
                                       // Identity palette mapping for indices 0..15.
        for i in 0..16 {
            self.attribute[i] = i as u8;
        }

        // Sequencer memory mode: chain-4 disabled (bit3 = 0) and odd/even enabled
        // (odd/even disable bit2 = 0).
        self.sequencer[4] = 0x02;
        // Sequencer map mask: enable planes 0 and 1 for text.
        self.sequencer[2] = 0x03;

        // Graphics controller misc: memory map = 0b11 => B8000, and odd/even.
        self.graphics[6] = 0x0C; // bits 2-3 = 3 (B8000)
        self.graphics[5] = 0x10; // set odd/even (bit4)
        self.graphics[4] = 0x00; // read map select

        // Cursor: enable, full block by default at 0.
        self.crtc[0x0A] = 0x00;
        self.crtc[0x0B] = 0x0F;
        self.crtc[0x0E] = 0x00;
        self.crtc[0x0F] = 0x00;

        self.vbe.enable = 0;
        self.ensure_buffers(80 * 9, 25 * 16);
        self.dirty = true;
    }

    /// Convenience helper: configure registers for VGA mode 13h (320x200x256).
    pub fn set_mode_13h(&mut self) {
        // Attribute mode control: graphics enable.
        self.attribute[0x10] = 0x01;
        // Identity palette mapping for indices 0..15; in 256-color mode the mapping is bypassed.
        for i in 0..16 {
            self.attribute[i] = i as u8;
        }

        // Sequencer memory mode: enable chain-4 (bit3) and disable odd/even (bit2).
        // The commonly used VGA register table for mode 13h programs 0x0E here.
        self.sequencer[4] = 0x0E;
        self.sequencer[2] = 0x0F; // enable all planes

        // Graphics controller misc: memory map = 0b01 => A0000 64KB.
        self.graphics[6] = 0x04;
        self.graphics[5] = 0x40; // 256-color shift register (bit6), no odd/even
        self.graphics[4] = 0x00;

        self.vbe.enable = 0;
        self.ensure_buffers(320, 200);
        self.dirty = true;
    }

    /// Convenience helper: enable a VBE linear mode (Bochs VBE_DISPI).
    pub fn set_svga_mode(&mut self, width: u16, height: u16, bpp: u16, lfb: bool) {
        self.vbe.xres = width;
        self.vbe.yres = height;
        self.vbe.bpp = bpp;
        self.vbe.virt_width = width;
        self.vbe.virt_height = height;
        self.vbe.x_offset = 0;
        self.vbe.y_offset = 0;
        self.vbe.bank = 0;

        self.vbe.enable = 0x0001 | if lfb { 0x0040 } else { 0 };
        self.ensure_buffers(width as u32, height as u32);
        self.dirty = true;
    }

    pub fn vram(&self) -> &[u8] {
        &self.vram
    }

    pub fn vram_mut(&mut self) -> &mut [u8] {
        self.dirty = true;
        &mut self.vram
    }

    /// Reads from guest physical memory, covering legacy VGA windows and the VBE linear framebuffer.
    pub fn mem_read_u8(&mut self, paddr: u32) -> u8 {
        if let Some(offset) = self.map_svga_lfb(paddr) {
            return self.vram.get(offset).copied().unwrap_or(0);
        }

        if self.vbe.enabled() {
            if let Some(offset) = self.map_svga_bank_window(paddr) {
                return self.vram.get(offset).copied().unwrap_or(0);
            }
        }

        if let Some(access) = self.map_legacy_vga(paddr) {
            match access {
                LegacyReadTarget::Single { plane, off } => {
                    return self.vram[plane * VGA_PLANE_SIZE + off];
                }
                LegacyReadTarget::Planar { off } => {
                    return self.read_u8_planar(off);
                }
            }
        }

        0
    }

    /// Writes to guest physical memory, covering legacy VGA windows and the VBE linear framebuffer.
    pub fn mem_write_u8(&mut self, paddr: u32, value: u8) {
        if let Some(offset) = self.map_svga_lfb(paddr) {
            if let Some(byte) = self.vram.get_mut(offset) {
                *byte = value;
                self.dirty = true;
            }
            return;
        }

        if self.vbe.enabled() {
            if let Some(offset) = self.map_svga_bank_window(paddr) {
                if let Some(byte) = self.vram.get_mut(offset) {
                    *byte = value;
                    self.dirty = true;
                }
                return;
            }
        }

        if let Some(access) = self.legacy_vga_write_targets(paddr) {
            match access {
                LegacyWriteTargets::Single { plane, off } => {
                    self.vram[plane * VGA_PLANE_SIZE + off] = value;
                }
                LegacyWriteTargets::Planar { off } => {
                    self.write_u8_planar(off, value);
                }
            }
            self.dirty = true;
        }
    }

    fn map_svga_lfb(&self, paddr: u32) -> Option<usize> {
        if !self.vbe.enabled() || !self.vbe.lfb_enabled() {
            return None;
        }
        let start = SVGA_LFB_BASE;
        let end = start.checked_add(self.vram.len() as u32)?;
        if paddr >= start && paddr < end {
            Some((paddr - start) as usize)
        } else {
            None
        }
    }

    fn map_svga_bank_window(&self, paddr: u32) -> Option<usize> {
        // Traditional banked window is a 64KiB aperture at A0000.
        let start = 0xA0000;
        let end = 0xB0000;
        if paddr < start || paddr >= end {
            return None;
        }
        let window_off = (paddr - start) as usize;
        let bank_base = (self.vbe.bank as usize) * 64 * 1024;
        bank_base.checked_add(window_off)
    }

    fn legacy_memory_map(&self) -> u8 {
        (self.graphics[6] >> 2) & 0x03
    }

    fn map_legacy_vga(&self, paddr: u32) -> Option<LegacyReadTarget> {
        let map = self.legacy_memory_map();
        let (base, size) = match map {
            0 => (0xA0000, 0x20000), // A0000-BFFFF
            1 => (0xA0000, 0x10000), // A0000-AFFFF
            2 => (0xB0000, 0x08000), // B0000-B7FFF
            3 => (0xB8000, 0x08000), // B8000-BFFFF
            _ => (0xA0000, 0x10000),
        };
        if paddr < base || paddr >= base + size {
            return None;
        }
        let off = (paddr - base) as usize;

        if self.chain4_enabled() {
            let plane = off & 0x03;
            let plane_off = off >> 2;
            Some(LegacyReadTarget::Single {
                plane,
                off: plane_off,
            })
        } else if self.odd_even_enabled() {
            let plane = off & 0x01;
            let plane_off = off >> 1;
            Some(LegacyReadTarget::Single {
                plane,
                off: plane_off,
            })
        } else {
            Some(LegacyReadTarget::Planar { off })
        }
    }

    fn legacy_vga_write_targets(&self, paddr: u32) -> Option<LegacyWriteTargets> {
        let map = self.legacy_memory_map();
        let (base, size) = match map {
            0 => (0xA0000, 0x20000), // A0000-BFFFF
            1 => (0xA0000, 0x10000), // A0000-AFFFF
            2 => (0xB0000, 0x08000), // B0000-B7FFF
            3 => (0xB8000, 0x08000), // B8000-BFFFF
            _ => (0xA0000, 0x10000),
        };
        if paddr < base || paddr >= base + size {
            return None;
        }
        let off = (paddr - base) as usize;

        if self.chain4_enabled() {
            let plane = off & 0x03;
            let plane_off = off >> 2;
            Some(LegacyWriteTargets::Single {
                plane,
                off: plane_off,
            })
        } else if self.odd_even_enabled() {
            let plane = off & 0x01;
            let plane_off = off >> 1;
            Some(LegacyWriteTargets::Single {
                plane,
                off: plane_off,
            })
        } else {
            Some(LegacyWriteTargets::Planar { off })
        }
    }

    fn plane_offset(&self, off: usize) -> usize {
        // VGA planes are 64KiB. Some memory map configurations expose a 128KiB window; on real
        // hardware the address decode effectively wraps, so we do the same.
        off & (VGA_PLANE_SIZE - 1)
    }

    fn load_latches(&mut self, off: usize) {
        let off = self.plane_offset(off);
        for plane in 0..4 {
            self.latches[plane] = self.vram[plane * VGA_PLANE_SIZE + off];
        }
    }

    fn read_u8_planar(&mut self, off: usize) -> u8 {
        let off = self.plane_offset(off);
        self.load_latches(off);
        let plane = (self.graphics[4] & 0x03) as usize;
        self.latches[plane]
    }

    fn write_u8_planar(&mut self, off: usize, value: u8) {
        let off = self.plane_offset(off);

        let write_mode = self.graphics[5] & 0x03;
        if write_mode != 1 {
            // VGA implements read-modify-write via latches for most write modes.
            self.load_latches(off);
        }

        let data_rotate = self.graphics[3];
        let rotate_count = data_rotate & 0x07;
        let func_select = (data_rotate >> 3) & 0x03;
        let bit_mask = self.graphics[8];

        let rotated = value.rotate_right(rotate_count as u32);

        let map_mask = self.sequencer[2] & 0x0F;
        let set_reset = self.graphics[0];
        let enable_set_reset = self.graphics[1];

        for plane in 0..4 {
            let plane_mask_bit = 1u8 << plane;
            if (map_mask & plane_mask_bit) == 0 {
                continue;
            }

            let latch = self.latches[plane];
            let result = match write_mode {
                0 => {
                    let mut data = rotated;
                    if (enable_set_reset & plane_mask_bit) != 0 {
                        data = if (set_reset & plane_mask_bit) != 0 {
                            0xFF
                        } else {
                            0x00
                        };
                    }

                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };

                    (alu & bit_mask) | (latch & !bit_mask)
                }
                1 => latch,
                2 => {
                    let data = if (value & plane_mask_bit) != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };
                    (alu & bit_mask) | (latch & !bit_mask)
                }
                3 => {
                    let data = if (set_reset & plane_mask_bit) != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };
                    let mask = bit_mask & rotated;
                    (alu & mask) | (latch & !mask)
                }
                _ => unreachable!("VGA write mode {write_mode} is invalid"),
            };

            self.vram[plane * VGA_PLANE_SIZE + off] = result;
        }
    }

    fn chain4_enabled(&self) -> bool {
        (self.sequencer[4] & 0x08) != 0
    }

    fn odd_even_enabled(&self) -> bool {
        // Odd/even requires the graphics controller bit plus the sequencer not disabling it.
        (self.graphics[5] & 0x10) != 0 && (self.sequencer[4] & 0x04) == 0
    }

    fn derived_render_mode(&self) -> RenderMode {
        if self.vbe.enabled() {
            return RenderMode::SvgaLinear {
                width: self.vbe.xres as u32,
                height: self.vbe.yres as u32,
                bpp: self.vbe.bpp,
            };
        }

        let attr_mode = self.attribute[0x10];
        let graphics_enabled = (attr_mode & 0x01) != 0;

        if !graphics_enabled {
            return RenderMode::Text80x25;
        }

        if self.chain4_enabled() {
            return RenderMode::Mode13h;
        }

        let (width, height) = self.derive_crtc_resolution();
        RenderMode::Planar4bpp { width, height }
    }

    fn derive_crtc_resolution(&self) -> (u32, u32) {
        // Horizontal display end is in character clocks (8 pixels).
        let width = (self.crtc[1] as u32 + 1) * 8;

        // Vertical display end is extended using overflow bits.
        let vde_low = self.crtc[0x12] as u32;
        let overflow = self.crtc[0x07];
        let vde =
            vde_low | (((overflow as u32 >> 1) & 1) << 8) | (((overflow as u32 >> 6) & 1) << 9);
        let height = vde + 1;

        // Clamp to something sane to avoid accidental huge allocations.
        let width = width.clamp(1, 2048);
        let height = height.clamp(1, 1536);
        (width, height)
    }

    fn ensure_buffers(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height && !self.front.is_empty() {
            return;
        }
        self.width = width;
        self.height = height;
        let pixels = width as usize * height as usize;
        self.front.resize(pixels, 0);
        self.back.resize(pixels, 0);
    }

    fn render(&mut self) {
        let mode = self.derived_render_mode();
        match mode {
            RenderMode::Text80x25 => {
                self.ensure_buffers(80 * 9, 25 * 16);
                self.render_text_mode();
            }
            RenderMode::Mode13h => {
                self.ensure_buffers(320, 200);
                self.render_mode_13h();
            }
            RenderMode::Planar4bpp { width, height } => {
                self.ensure_buffers(width, height);
                self.render_planar_4bpp(width, height);
            }
            RenderMode::SvgaLinear { width, height, bpp } => {
                self.ensure_buffers(width, height);
                self.render_svga(width, height, bpp);
            }
        }
    }

    fn render_text_mode(&mut self) {
        self.back.fill(0);
        let cols = 80usize;
        let rows = 25usize;
        let cell_w = 9usize;
        let cell_h = 16usize;
        let width = self.width as usize;

        let line_graphics_enable = (self.attribute[0x10] & (1 << 2)) != 0;
        let blink_enabled = (self.attribute[0x10] & (1 << 3)) != 0;

        for row in 0..rows {
            for col in 0..cols {
                let cell_index = row * cols + col;
                let ch = self.vram[cell_index];
                let attr = self.vram[VGA_PLANE_SIZE + cell_index];

                let fg = attr & 0x0F;
                let bg = if blink_enabled {
                    (attr >> 4) & 0x07
                } else {
                    (attr >> 4) & 0x0F
                };

                let fg_dac = self.attribute_palette_lookup(fg);
                let bg_dac = self.attribute_palette_lookup(bg);

                let fg_px = rgb_to_rgba_u32(self.dac[fg_dac as usize]);
                let bg_px = rgb_to_rgba_u32(self.dac[bg_dac as usize]);

                for y in 0..cell_h {
                    let glyph_row = self.font_row_8x16(ch, y as u8);
                    let dst_y = row * cell_h + y;
                    let dst_row_base = dst_y * width + col * cell_w;

                    for x in 0..8 {
                        let bit = (glyph_row >> (7 - x)) & 1;
                        self.back[dst_row_base + x] = if bit != 0 { fg_px } else { bg_px };
                    }

                    // 9th column: replicate for box drawing range when enabled; otherwise background.
                    let ninth_bit = if line_graphics_enable && (0xC0..=0xDF).contains(&ch) {
                        glyph_row & 1
                    } else {
                        0
                    };
                    self.back[dst_row_base + 8] = if ninth_bit != 0 { fg_px } else { bg_px };
                }

                // Cursor overlay.
                if self.cursor_visible_at(cell_index as u16) {
                    let (start, end) = self.cursor_scanlines();
                    if start <= end {
                        for y in start..=end {
                            if y >= cell_h as u8 {
                                continue;
                            }
                            let dst_y = row * cell_h + y as usize;
                            let dst_row_base = dst_y * width + col * cell_w;
                            for x in 0..cell_w {
                                let px = &mut self.back[dst_row_base + x];
                                *px = if *px == fg_px { bg_px } else { fg_px };
                            }
                        }
                    }
                }
            }
        }
    }

    fn cursor_visible_at(&self, cell_index: u16) -> bool {
        // Cursor disable bit is bit5 of cursor start register.
        if (self.crtc[0x0A] & 0x20) != 0 {
            return false;
        }
        let cursor_pos = ((self.crtc[0x0E] as u16) << 8) | self.crtc[0x0F] as u16;
        cursor_pos == cell_index
    }

    fn cursor_scanlines(&self) -> (u8, u8) {
        let start = self.crtc[0x0A] & 0x1F;
        let end = self.crtc[0x0B] & 0x1F;
        (start, end)
    }

    fn font_row_8x16(&self, ch: u8, row: u8) -> u8 {
        let row8 = (row / 2) as usize;
        FONT8X8_BASIC.get(ch as usize).copied().unwrap_or([0; 8])[row8]
    }

    fn attribute_palette_lookup(&self, color: u8) -> u8 {
        // Attribute palette registers are 6-bit and feed into the DAC.
        let idx = (color & 0x0F) as usize;
        self.attribute[idx] & 0x3F
    }

    fn render_mode_13h(&mut self) {
        let width = 320usize;
        let height = 200usize;
        self.back.fill(0);
        for y in 0..height {
            for x in 0..width {
                let linear = y * width + x;
                let plane = linear & 3;
                let off = linear >> 2;
                let idx = self.vram[plane * VGA_PLANE_SIZE + off];
                let color = self.dac[(idx & self.pel_mask) as usize];
                self.back[linear] = rgb_to_rgba_u32(color);
            }
        }
    }

    fn render_planar_4bpp(&mut self, width: u32, height: u32) {
        self.back.fill(0);
        let width_usize = width as usize;
        let height_usize = height as usize;
        let bytes_per_line = width_usize.div_ceil(8);

        for y in 0..height_usize {
            for x in 0..width_usize {
                let byte_index = y * bytes_per_line + (x / 8);
                let bit = 7 - (x & 7);
                let mut color = 0u8;
                for plane in 0..4 {
                    let b = self.vram[plane * VGA_PLANE_SIZE + byte_index];
                    let v = (b >> bit) & 1;
                    color |= v << plane;
                }
                let dac_idx = self.attribute_palette_lookup(color);
                self.back[y * width_usize + x] = rgb_to_rgba_u32(self.dac[dac_idx as usize]);
            }
        }
    }

    fn render_svga(&mut self, width: u32, height: u32, bpp: u16) {
        self.back.fill(0);
        let stride_pixels = self.vbe.effective_stride_pixels();
        let x_off = self.vbe.x_offset as u32;
        let y_off = self.vbe.y_offset as u32;

        let bytes_per_pixel = match bpp {
            32 => 4,
            24 => 3,
            16 | 15 => 2,
            8 => 1,
            _ => 4,
        } as u32;

        let stride_bytes = stride_pixels.saturating_mul(bytes_per_pixel) as usize;
        for y in 0..height {
            let src_y = y_off + y;
            let dst_row = (y * width) as usize;
            let src_row_base = (src_y as usize).saturating_mul(stride_bytes);
            for x in 0..width {
                let src_x = x_off + x;
                let src = src_row_base + (src_x as usize).saturating_mul(bytes_per_pixel as usize);
                let px = match bpp {
                    32 => {
                        // VBE packed pixels: little-endian B,G,R,X
                        let b = *self.vram.get(src).unwrap_or(&0);
                        let g = *self.vram.get(src + 1).unwrap_or(&0);
                        let r = *self.vram.get(src + 2).unwrap_or(&0);
                        rgb_to_rgba_u32(Rgb { r, g, b })
                    }
                    24 => {
                        let b = *self.vram.get(src).unwrap_or(&0);
                        let g = *self.vram.get(src + 1).unwrap_or(&0);
                        let r = *self.vram.get(src + 2).unwrap_or(&0);
                        rgb_to_rgba_u32(Rgb { r, g, b })
                    }
                    16 => {
                        let lo = *self.vram.get(src).unwrap_or(&0) as u16;
                        let hi = *self.vram.get(src + 1).unwrap_or(&0) as u16;
                        let v = lo | (hi << 8);
                        let r = ((v >> 11) & 0x1F) as u8;
                        let g = ((v >> 5) & 0x3F) as u8;
                        let b = (v & 0x1F) as u8;
                        rgb_to_rgba_u32(Rgb {
                            r: (r << 3) | (r >> 2),
                            g: (g << 2) | (g >> 4),
                            b: (b << 3) | (b >> 2),
                        })
                    }
                    15 => {
                        let lo = *self.vram.get(src).unwrap_or(&0) as u16;
                        let hi = *self.vram.get(src + 1).unwrap_or(&0) as u16;
                        let v = lo | (hi << 8);
                        let r = ((v >> 10) & 0x1F) as u8;
                        let g = ((v >> 5) & 0x1F) as u8;
                        let b = (v & 0x1F) as u8;
                        rgb_to_rgba_u32(Rgb {
                            r: (r << 3) | (r >> 2),
                            g: (g << 3) | (g >> 2),
                            b: (b << 3) | (b >> 2),
                        })
                    }
                    8 => {
                        let idx = *self.vram.get(src).unwrap_or(&0);
                        rgb_to_rgba_u32(self.dac[(idx & self.pel_mask) as usize])
                    }
                    _ => 0,
                };
                self.back[dst_row + x as usize] = px;
            }
        }
    }

    fn write_dac_data(&mut self, value: u8) {
        let idx = self.dac_write_index as usize;
        let component = self.dac_write_subindex;
        let v = palette::vga_6bit_to_8bit(value & 0x3F);
        match component {
            0 => self.dac[idx].r = v,
            1 => self.dac[idx].g = v,
            2 => self.dac[idx].b = v,
            _ => {}
        }
        self.dac_write_subindex = (self.dac_write_subindex + 1) % 3;
        if self.dac_write_subindex == 0 {
            self.dac_write_index = self.dac_write_index.wrapping_add(1);
        }
        self.dirty = true;
    }

    fn read_dac_data(&mut self) -> u8 {
        let idx = self.dac_read_index as usize;
        let component = self.dac_read_subindex;
        let v = match component {
            0 => palette::vga_8bit_to_6bit(self.dac[idx].r),
            1 => palette::vga_8bit_to_6bit(self.dac[idx].g),
            2 => palette::vga_8bit_to_6bit(self.dac[idx].b),
            _ => 0,
        };
        self.dac_read_subindex = (self.dac_read_subindex + 1) % 3;
        if self.dac_read_subindex == 0 {
            self.dac_read_index = self.dac_read_index.wrapping_add(1);
        }
        v
    }

    fn vbe_read_reg(&self, index: u16) -> u16 {
        match index {
            0x0000 => 0xB0C5, // ID
            0x0001 => self.vbe.xres,
            0x0002 => self.vbe.yres,
            0x0003 => self.vbe.bpp,
            0x0004 => self.vbe.enable,
            0x0005 => self.vbe.bank,
            0x0006 => self.vbe.virt_width,
            0x0007 => self.vbe.virt_height,
            0x0008 => self.vbe.x_offset,
            0x0009 => self.vbe.y_offset,
            0x000A => (self.vram.len() / (64 * 1024)) as u16,
            _ => 0,
        }
    }

    fn vbe_write_reg(&mut self, index: u16, value: u16) {
        match index {
            0x0001 => self.vbe.xres = value,
            0x0002 => self.vbe.yres = value,
            0x0003 => self.vbe.bpp = value,
            0x0004 => {
                self.vbe.enable = value;
            }
            0x0005 => self.vbe.bank = value,
            0x0006 => self.vbe.virt_width = value,
            0x0007 => self.vbe.virt_height = value,
            0x0008 => self.vbe.x_offset = value,
            0x0009 => self.vbe.y_offset = value,
            _ => {}
        }
        self.dirty = true;
    }
}

#[derive(Debug, Clone, Copy)]
enum LegacyWriteTargets {
    Single { plane: usize, off: usize },
    Planar { off: usize },
}

#[derive(Debug, Clone, Copy)]
enum LegacyReadTarget {
    Single { plane: usize, off: usize },
    Planar { off: usize },
}

impl DisplayOutput for VgaDevice {
    fn get_framebuffer(&self) -> &[u32] {
        &self.front
    }

    fn get_resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn present(&mut self) {
        if !self.dirty {
            return;
        }
        self.render();
        std::mem::swap(&mut self.front, &mut self.back);
        self.dirty = false;
    }
}

impl PortIO for VgaDevice {
    fn port_read(&mut self, port: u16, size: usize) -> u32 {
        match (port, size) {
            // VGA misc output.
            (0x3CC, 1) => self.misc_output as u32,

            // Sequencer.
            (0x3C4, 1) => self.sequencer_index as u32,
            (0x3C5, 1) => {
                let idx = (self.sequencer_index as usize) % self.sequencer.len();
                self.sequencer[idx] as u32
            }

            // Graphics controller.
            (0x3CE, 1) => self.graphics_index as u32,
            (0x3CF, 1) => {
                let idx = (self.graphics_index as usize) % self.graphics.len();
                self.graphics[idx] as u32
            }

            // CRTC.
            (0x3D4, 1) => self.crtc_index as u32,
            (0x3D5, 1) => {
                let idx = (self.crtc_index as usize) % self.crtc.len();
                self.crtc[idx] as u32
            }

            // Attribute controller data read (index written via 0x3C0).
            (0x3C1, 1) => {
                let idx = (self.attribute_index as usize) % self.attribute.len();
                self.attribute[idx] as u32
            }

            // Input status 1. Reading resets the attribute flip-flop.
            (0x3DA, 1) => {
                self.attribute_flip_flop_data = false;
                self.input_status1_vretrace = !self.input_status1_vretrace;
                let v = if self.input_status1_vretrace {
                    0x08
                } else {
                    0x00
                };
                // Bit 3: vertical retrace. Bit 0: display enable (rough approximation).
                (v | (v >> 3)) as u32
            }

            // DAC.
            (0x3C6, 1) => self.pel_mask as u32,
            (0x3C7, 1) => self.dac_read_index as u32,
            (0x3C8, 1) => self.dac_write_index as u32,
            (0x3C9, 1) => self.read_dac_data() as u32,

            // Bochs VBE.
            (0x01CE, 2) => self.vbe_index as u32,
            (0x01CF, 2) => self.vbe_read_reg(self.vbe_index) as u32,

            _ => 0,
        }
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        match (port, size) {
            // VGA misc output.
            (0x3C2, 1) => {
                self.misc_output = val as u8;
                self.dirty = true;
            }

            // Sequencer.
            (0x3C4, 1) => self.sequencer_index = val as u8,
            (0x3C5, 1) => {
                let idx = (self.sequencer_index as usize) % self.sequencer.len();
                self.sequencer[idx] = val as u8;
                self.dirty = true;
            }

            // Graphics controller.
            (0x3CE, 1) => self.graphics_index = val as u8,
            (0x3CF, 1) => {
                let idx = (self.graphics_index as usize) % self.graphics.len();
                self.graphics[idx] = val as u8;
                self.dirty = true;
            }

            // CRTC.
            (0x3D4, 1) => self.crtc_index = val as u8,
            (0x3D5, 1) => {
                let idx = (self.crtc_index as usize) % self.crtc.len();
                if idx <= 0x07 && (self.crtc.get(0x11).copied().unwrap_or(0) & 0x80) != 0 {
                    return;
                }
                self.crtc[idx] = val as u8;
                self.dirty = true;
            }

            // Attribute controller (index/data with flip-flop).
            (0x3C0, 1) => {
                let v = val as u8;
                if !self.attribute_flip_flop_data {
                    self.attribute_index = v & 0x1F;
                    self.attribute_flip_flop_data = true;
                } else {
                    let idx = (self.attribute_index as usize) % self.attribute.len();
                    self.attribute[idx] = v;
                    self.attribute_flip_flop_data = false;
                    self.dirty = true;
                }
            }

            // DAC.
            (0x3C6, 1) => {
                self.pel_mask = val as u8;
                self.dirty = true;
            }
            (0x3C7, 1) => {
                self.dac_read_index = val as u8;
                self.dac_read_subindex = 0;
            }
            (0x3C8, 1) => {
                self.dac_write_index = val as u8;
                self.dac_write_subindex = 0;
            }
            (0x3C9, 1) => self.write_dac_data(val as u8),

            // Bochs VBE.
            (0x01CE, 2) => self.vbe_index = (val & 0xFFFF) as u16,
            (0x01CF, 2) => self.vbe_write_reg(self.vbe_index, (val & 0xFFFF) as u16),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a64(bytes: &[u8]) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x00000100000001B3;
        let mut hash = FNV_OFFSET;
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    fn framebuffer_hash(dev: &VgaDevice) -> u64 {
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(dev.front.as_ptr() as *const u8, dev.front.len() * 4)
        };
        fnv1a64(bytes)
    }

    #[test]
    fn attribute_flip_flop_resets_on_input_status_read() {
        let mut dev = VgaDevice::new();

        // First write selects attribute index and flips to "data" state.
        dev.port_write(0x3C0, 1, 0x10);
        assert!(dev.attribute_flip_flop_data);

        // Reading input status should reset back to "index" state.
        dev.port_read(0x3DA, 1);
        assert!(!dev.attribute_flip_flop_data);

        // Now writing to 0x3C0 should be treated as an index write again.
        dev.port_write(0x3C0, 1, 0x11);
        assert_eq!(dev.attribute_index, 0x11);
        assert!(dev.attribute_flip_flop_data);
    }

    #[test]
    fn text_mode_golden_hash() {
        let mut dev = VgaDevice::new();
        dev.set_text_mode_80x25();

        // Disable cursor for deterministic output.
        dev.crtc[0x0A] = 0x20;

        // Write "A" in the top-left cell with light grey on blue.
        let base = 0xB8000u32;
        dev.mem_write_u8(base, b'A');
        dev.mem_write_u8(base + 1, 0x1F);

        dev.present();
        assert_eq!(dev.get_resolution(), (720, 400));
        assert_eq!(framebuffer_hash(&dev), 0x5cfe440e33546065);
    }

    #[test]
    fn mode13h_golden_hash() {
        let mut dev = VgaDevice::new();
        dev.set_mode_13h();

        // Fill the 64k window with a repeating ramp.
        let base = 0xA0000u32;
        for i in 0..(320 * 200) {
            dev.mem_write_u8(base + i as u32, (i & 0xFF) as u8);
        }

        dev.present();
        assert_eq!(dev.get_resolution(), (320, 200));
        assert_eq!(framebuffer_hash(&dev), 0xf54b1d9c21a2a115);
    }

    #[test]
    fn register_writes_switch_to_mode13h() {
        let mut dev = VgaDevice::new();

        // Attribute controller: set graphics enable bit in mode control (index 0x10).
        dev.port_read(0x3DA, 1); // reset flip-flop
        dev.port_write(0x3C0, 1, 0x10);
        dev.port_write(0x3C0, 1, 0x01);

        // Sequencer: enable chain4 in memory mode (index 4).
        dev.port_write(0x3C4, 1, 0x04);
        dev.port_write(0x3C5, 1, 0x08);

        // Graphics controller: map to A0000 64KiB (index 6, bits 2-3 = 01).
        dev.port_write(0x3CE, 1, 0x06);
        dev.port_write(0x3CF, 1, 0x04);

        dev.present();
        assert_eq!(dev.get_resolution(), (320, 200));
    }

    #[test]
    fn vbe_linear_framebuffer_write_shows_up_in_output() {
        let mut dev = VgaDevice::new();

        // 64x64x32bpp, LFB enabled.
        dev.port_write(0x01CE, 2, 0x0001);
        dev.port_write(0x01CF, 2, 64);
        dev.port_write(0x01CE, 2, 0x0002);
        dev.port_write(0x01CF, 2, 64);
        dev.port_write(0x01CE, 2, 0x0003);
        dev.port_write(0x01CF, 2, 32);
        dev.port_write(0x01CE, 2, 0x0004);
        dev.port_write(0x01CF, 2, 0x0041);

        // Write a red pixel at (0,0) in BGRX format.
        dev.mem_write_u8(SVGA_LFB_BASE, 0x00); // B
        dev.mem_write_u8(SVGA_LFB_BASE + 1, 0x00); // G
        dev.mem_write_u8(SVGA_LFB_BASE + 2, 0xFF); // R
        dev.mem_write_u8(SVGA_LFB_BASE + 3, 0x00); // X

        dev.present();
        assert_eq!(dev.get_resolution(), (64, 64));
        assert_eq!(dev.get_framebuffer()[0], 0xFF00_00FF);
    }

    #[test]
    fn planar_write_mode0_set_reset_writes_selected_planes() {
        let mut dev = VgaDevice::new();

        // Configure a basic planar graphics window at A0000.
        dev.sequencer[4] = 0x00; // chain4 disabled, odd/even disabled
        dev.sequencer[2] = 0x0F; // enable all planes (map mask)

        dev.graphics[6] = 0x04; // memory map 0b01 => A0000 64KiB
        dev.graphics[5] = 0x00; // write mode 0, odd/even off
        dev.graphics[3] = 0x00; // rotate=0, func=replace
        dev.graphics[8] = 0xFF; // bit mask
        dev.graphics[0] = 0b0101; // set/reset: planes 0 and 2 set
        dev.graphics[1] = 0x0F; // enable set/reset for all planes

        dev.mem_write_u8(0xA0000, 0xAA);

        assert_eq!(dev.vram[0], 0xFF);
        assert_eq!(dev.vram[VGA_PLANE_SIZE], 0x00);
        assert_eq!(dev.vram[2 * VGA_PLANE_SIZE], 0xFF);
        assert_eq!(dev.vram[3 * VGA_PLANE_SIZE], 0x00);
    }

    #[test]
    fn planar_write_mode0_applies_bit_mask_and_latches() {
        let mut dev = VgaDevice::new();

        dev.sequencer[4] = 0x00;
        dev.sequencer[2] = 0x01; // plane 0 only

        dev.graphics[6] = 0x04; // A0000 64KiB
        dev.graphics[5] = 0x00; // write mode 0
        dev.graphics[3] = 0x00; // replace
        dev.graphics[8] = 0x0F; // only lower nibble affected
        dev.graphics[0] = 0x00; // set/reset disabled
        dev.graphics[1] = 0x00;

        // Seed destination byte so we can observe latch+mask behavior.
        dev.vram[0] = 0xA0;

        dev.mem_write_u8(0xA0000, 0x05);

        assert_eq!(dev.vram[0], 0xA5);
    }
}
