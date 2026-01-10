mod modeset;
mod mmio;
mod ports;
mod regs;
pub mod dac;
pub mod edid;
pub mod memory;
pub mod render;
pub mod vbe;

use crate::io::PortIO;

pub use dac::VgaDac;
pub use memory::{VgaMemory, VramPlane, VGA_PLANE_SIZE};
pub use mmio::{VgaMmio, VgaMmioRegion, VGA_BANK_WINDOW_PADDR, VGA_BANK_WINDOW_SIZE};
pub use ports::VgaDevice;
pub use regs::{VgaDerivedState, VgaPlanarShift};
pub use vbe::{VbeControllerInfo, VbeModeInfo, VbeState, VBE_BIOS_DATA_PADDR, VBE_LFB_BASE, VBE_LFB_SIZE};

pub use render::mode13h::{Mode13hRenderer, MODE13H_HEIGHT, MODE13H_VRAM_SIZE, MODE13H_WIDTH};
pub use render::planar16::{Mode12hRenderer, MODE12H_HEIGHT, MODE12H_WIDTH};
pub use render::text::{TextModeRenderer, TEXT_MODE_HEIGHT, TEXT_MODE_WIDTH};

/// VGA graphics memory base address (A000:0000).
pub const VRAM_BASE: u32 = 0xA0000;
pub const PLANE_SIZE: usize = 0x10000;
pub const VRAM_SIZE: usize = PLANE_SIZE * 4;

/// VGA render modes that this crate can currently rasterize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VgaDetectedMode {
    TextMode,
    Mode13h,
    Mode12h,
}

impl VgaDetectedMode {
    /// Best-effort heuristic based on [`VgaDevice`] register state.
    ///
    /// This intentionally errs on the side of "unknown" until we have a fuller model
    /// of the CRTC timing registers.
    pub fn detect(regs: &VgaDevice) -> Option<Self> {
        let derived = regs.derived_state();

        if !derived.is_graphics {
            return Some(Self::TextMode);
        }

        if derived.is_graphics && derived.chain4 && !derived.odd_even && derived.bpp_guess == 8 {
            return Some(Self::Mode13h);
        }

        // 640x480x16 planar (Mode 12h) heuristics:
        // - 4bpp planar mode (no chain4, odd/even disabled)
        // - bytes/scanline = 80 (CRTC offset = 40 words)
        // - vertical display end = 479 (height 480)
        if derived.is_graphics
            && !derived.chain4
            && !derived.odd_even
            && derived.bpp_guess == 4
            && matches!(derived.planar_shift, VgaPlanarShift::None)
        {
            let offset_words = regs.crtc_regs.get(0x13).copied().unwrap_or(0) as usize;
            let bytes_per_scanline = offset_words * 2;
            let width = bytes_per_scanline * 8;

            let vde_low = regs.crtc_regs.get(0x12).copied().unwrap_or(0) as u16;
            let overflow = regs.crtc_regs.get(0x07).copied().unwrap_or(0) as u16;
            let vde = vde_low | ((overflow & 0x02) << 7) | ((overflow & 0x40) << 3);
            let height = usize::from(vde.saturating_add(1));

            if width == MODE12H_WIDTH && height == MODE12H_HEIGHT {
                return Some(Self::Mode12h);
            }
        }

        None
    }
}

/// Render entrypoint that selects an appropriate rasterizer based on VGA register state.
#[derive(Debug)]
pub struct VgaRenderer {
    text: TextModeRenderer,
    mode13h: Mode13hRenderer,
    mode12h: Mode12hRenderer,
}

impl Default for VgaRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl VgaRenderer {
    pub fn new() -> Self {
        Self {
            text: TextModeRenderer::new(),
            mode13h: Mode13hRenderer::new(),
            mode12h: Mode12hRenderer::new(),
        }
    }

    /// Renders the current VGA mode (if supported) into an RGBA8888 framebuffer
    /// (`u32::from_le_bytes([r, g, b, a])`).
    ///
    /// Returns `(width, height, framebuffer)` on success.
    pub fn render<'a>(
        &'a mut self,
        regs: &VgaDevice,
        vram: &mut VgaMemory,
        dac: &mut VgaDac,
    ) -> Option<(usize, usize, &'a [u32])> {
        match VgaDetectedMode::detect(regs)? {
            VgaDetectedMode::TextMode => {
                Some((TEXT_MODE_WIDTH, TEXT_MODE_HEIGHT, self.text.render(regs)))
            }
            VgaDetectedMode::Mode13h => Some((
                MODE13H_WIDTH,
                MODE13H_HEIGHT,
                self.mode13h.render(vram, dac),
            )),
            VgaDetectedMode::Mode12h => Some((
                MODE12H_WIDTH,
                MODE12H_HEIGHT,
                self.mode12h.render(regs, vram, dac),
            )),
        }
    }
}

/// Information needed to update the BIOS Data Area (BDA) after a legacy VGA
/// mode set (INT 10h/AH=00h style).
///
/// The VGA device does not write the BDA itself; firmware should call
/// [`VgaDevice::set_legacy_mode`] and then apply these values to BDA fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyBdaInfo {
    pub video_mode: u8,
    pub columns: u16,
    pub rows: u16,
    pub page_size: u16,
    pub text_base_segment: u16,
    /// Cursor position for pages 0..=7, encoded as (row << 8) | col.
    pub cursor_pos: [u16; 8],
    pub active_page: u8,
}

use crate::display::framebuffer::{FramebufferError, OwnedSharedFramebuffer};

pub struct VgaSharedFramebufferOutput {
    shared_framebuffer: OwnedSharedFramebuffer,
}

impl VgaSharedFramebufferOutput {
    pub fn new(max_width: u32, max_height: u32) -> Result<Self, FramebufferError> {
        let stride_bytes = max_width
            .checked_mul(4)
            .ok_or(FramebufferError::InvalidDimensions)?;
        let shared_framebuffer = OwnedSharedFramebuffer::new(max_width, max_height, stride_bytes)?;
        Ok(Self { shared_framebuffer })
    }

    pub fn ptr(&self) -> *const u8 {
        self.shared_framebuffer.ptr()
    }

    pub fn len_bytes(&self) -> usize {
        self.shared_framebuffer.len_bytes()
    }

    pub fn present_rgba8888(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), FramebufferError> {
        let stride_bytes = width
            .checked_mul(4)
            .ok_or(FramebufferError::InvalidDimensions)?;
        self.present_rgba8888_strided(width, height, stride_bytes, rgba)
    }

    pub fn present_rgba8888_strided(
        &mut self,
        width: u32,
        height: u32,
        stride_bytes: u32,
        rgba: &[u8],
    ) -> Result<(), FramebufferError> {
        let mut view = self.shared_framebuffer.view_mut();
        view.present_rgba8888(width, height, stride_bytes, rgba)
    }

    pub fn present_rgba8888_u32(
        &mut self,
        width: usize,
        height: usize,
        pixels: &[u32],
    ) -> Result<(), FramebufferError> {
        let expected = width
            .checked_mul(height)
            .ok_or(FramebufferError::InvalidDimensions)?;
        if pixels.len() < expected {
            return Err(FramebufferError::BufferTooSmall);
        }

        let bytes_len = expected
            .checked_mul(4)
            .ok_or(FramebufferError::InvalidDimensions)?;
        let bytes = unsafe { core::slice::from_raw_parts(pixels.as_ptr() as *const u8, bytes_len) };
        self.present_rgba8888(width as u32, height as u32, bytes)
    }
}

/// A minimal VGA device bundle (registers + VRAM + DAC) with a Mode 13h renderer.
///
/// This is intentionally scoped to what we need for early bring-up:
/// - register/port I/O via [`PortIO`]
/// - a packed-pixel Mode 13h rasterizer via [`VgaRenderer`]
/// - palette writes via the standard DAC ports (0x3C8/0x3C9) and PEL mask (0x3C6)
#[derive(Debug)]
pub struct Vga {
    regs: VgaDevice,
    vram: VgaMemory,
    dac: VgaDac,
    renderer: VgaRenderer,

    dac_write_index: u8,
    dac_write_component: u8,
    dac_write_latch: [u8; 3],
}

impl Default for Vga {
    fn default() -> Self {
        Self::new()
    }
}

impl Vga {
    pub fn new() -> Self {
        Self {
            regs: VgaDevice::new(),
            vram: VgaMemory::new(),
            dac: VgaDac::new(),
            renderer: VgaRenderer::new(),
            dac_write_index: 0,
            dac_write_component: 0,
            dac_write_latch: [0; 3],
        }
    }

    pub fn regs(&self) -> &VgaDevice {
        &self.regs
    }

    pub fn regs_mut(&mut self) -> &mut VgaDevice {
        &mut self.regs
    }

    pub fn dac(&self) -> &VgaDac {
        &self.dac
    }

    pub fn dac_mut(&mut self) -> &mut VgaDac {
        &mut self.dac
    }

    pub fn write_vram(&mut self, offset: usize, data: &[u8]) {
        self.vram.write(offset, data);
    }

    pub fn write_vram_u8(&mut self, offset: usize, value: u8) {
        self.vram.write(offset, &[value]);
    }

    pub fn render(&mut self) -> Option<(usize, usize, &[u32])> {
        self.renderer.render(&self.regs, &mut self.vram, &mut self.dac)
    }

    fn dac_port_write_u8(&mut self, port: u16, val: u8) {
        match port {
            // PEL mask.
            0x3C6 => {
                self.dac.set_pel_mask(val);
            }
            // DAC write index.
            0x3C8 => {
                self.dac_write_index = val;
                self.dac_write_component = 0;
            }
            // DAC data.
            0x3C9 => {
                let component = (val & 0x3F) as u8;
                let idx = (self.dac_write_component as usize).min(2);
                self.dac_write_latch[idx] = component;
                self.dac_write_component = self.dac_write_component.wrapping_add(1);
                if self.dac_write_component >= 3 {
                    self.dac.set_entry_6bit(
                        self.dac_write_index,
                        self.dac_write_latch[0],
                        self.dac_write_latch[1],
                        self.dac_write_latch[2],
                    );
                    self.dac_write_index = self.dac_write_index.wrapping_add(1);
                    self.dac_write_component = 0;
                }
            }
            _ => {}
        }
    }

    fn dac_port_read_u8(&self, port: u16) -> Option<u8> {
        match port {
            0x3C6 => Some(self.dac.pel_mask()),
            0x3C8 => Some(self.dac_write_index),
            0x3C9 => Some(0),
            _ => None,
        }
    }
}

impl PortIO for Vga {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        match size {
            1 => {
                if let Some(val) = self.dac_port_read_u8(port) {
                    u32::from(val)
                } else {
                    self.regs.port_read(port, 1)
                }
            }
            2 => {
                let lo = self.port_read(port, 1) as u8;
                let hi = self.port_read(port.wrapping_add(1), 1) as u8;
                u32::from(u16::from_le_bytes([lo, hi]))
            }
            4 => {
                let b0 = self.port_read(port, 1) as u8;
                let b1 = self.port_read(port.wrapping_add(1), 1) as u8;
                let b2 = self.port_read(port.wrapping_add(2), 1) as u8;
                let b3 = self.port_read(port.wrapping_add(3), 1) as u8;
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => 0,
        }
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        match size {
            1 => {
                let b0 = val as u8;
                if matches!(port, 0x3C6 | 0x3C8 | 0x3C9) {
                    self.dac_port_write_u8(port, b0);
                } else {
                    self.regs.port_write(port, 1, val);
                }
            }
            2 => {
                let [b0, b1] = (val as u16).to_le_bytes();
                self.port_write(port, 1, b0 as u32);
                self.port_write(port.wrapping_add(1), 1, b1 as u32);
            }
            4 => {
                let [b0, b1, b2, b3] = val.to_le_bytes();
                self.port_write(port, 1, b0 as u32);
                self.port_write(port.wrapping_add(1), 1, b1 as u32);
                self.port_write(port.wrapping_add(2), 1, b2 as u32);
                self.port_write(port.wrapping_add(3), 1, b3 as u32);
            }
            _ => {}
        }
    }
}
