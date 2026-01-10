mod modeset;
mod ports;
mod regs;

pub mod dac;
pub mod memory;
pub mod render;

pub use dac::VgaDac;
pub use memory::VgaMemory;
pub use ports::VgaDevice;
pub use regs::{VgaDerivedState, VgaPlanarShift};
pub use render::mode13h::{Mode13hRenderer, MODE13H_HEIGHT, MODE13H_VRAM_SIZE, MODE13H_WIDTH};

/// VGA render modes that this crate can currently rasterize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VgaDetectedMode {
    Mode13h,
}

impl VgaDetectedMode {
    /// Best-effort heuristic based on [`VgaDerivedState`].
    ///
    /// This intentionally errs on the side of "unknown" until we have a fuller model
    /// of the CRTC timing registers. For now, we only claim Mode 13h when the register
    /// set looks like a packed 256-colour chain-4 graphics mode.
    pub fn detect(derived: VgaDerivedState) -> Option<Self> {
        if derived.is_graphics && derived.chain4 && !derived.odd_even && derived.bpp_guess == 8 {
            Some(Self::Mode13h)
        } else {
            None
        }
    }
}

/// Render entrypoint that selects an appropriate rasterizer based on VGA register state.
#[derive(Debug)]
pub struct VgaRenderer {
    mode13h: Mode13hRenderer,
}

impl Default for VgaRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl VgaRenderer {
    pub fn new() -> Self {
        Self {
            mode13h: Mode13hRenderer::new(),
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
        match VgaDetectedMode::detect(regs.derived_state())? {
            VgaDetectedMode::Mode13h => {
                Some((MODE13H_WIDTH, MODE13H_HEIGHT, self.mode13h.render(vram, dac)))
            }
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

