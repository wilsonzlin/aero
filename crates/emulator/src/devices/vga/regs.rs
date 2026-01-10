#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VgaPlanarShift {
    /// Standard planar shift (text mode / planar graphics).
    None,
    /// 256-colour shift (packed/chain4-style).
    Shift256,
    /// Interleaved shift (packed pixels across planes).
    Interleaved,
}

impl Default for VgaPlanarShift {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VgaDerivedState {
    /// Heuristic: does the current register set describe a graphics mode?
    pub is_graphics: bool,
    /// VGA chain-4 addressing (SEQ Memory Mode bit 3).
    pub chain4: bool,
    /// Odd/even addressing enabled (SEQ Memory Mode bit 2 == 0).
    pub odd_even: bool,
    pub planar_shift: VgaPlanarShift,
    /// Best-effort guess; not authoritative.
    pub bpp_guess: u8,
}

pub(crate) const SEQ_REGS_INITIAL_LEN: usize = 5; // 0..=4
pub(crate) const GC_REGS_INITIAL_LEN: usize = 9; // 0..=8
pub(crate) const AC_REGS_INITIAL_LEN: usize = 0x15; // 0..=0x14
pub(crate) const CRTC_REGS_INITIAL_LEN: usize = 0x19; // 0..=0x18

// ---------------------------------------------------------------------------
// Reset / baseline register sets
// ---------------------------------------------------------------------------

/// Minimal power-on defaults (not a full hardware reset image).
pub(crate) const POWER_ON_MISC_OUTPUT: u8 = 0x01;

/// "BIOS set mode 3" baseline (80x25 text, colour).
///
/// These values are intentionally minimal and primarily serve as sane defaults
/// before we have a real VGA BIOS/mode table implementation.
pub(crate) const BIOS_MODE3_MISC_OUTPUT: u8 = 0x67;

pub(crate) const BIOS_MODE3_SEQ_REGS: [u8; SEQ_REGS_INITIAL_LEN] = [
    0x03, // Reset
    0x00, // Clocking Mode
    0x03, // Map Mask (planes 0-1 enabled for text)
    0x00, // Character Map Select
    0x02, // Memory Mode (odd/even enabled, chain4 disabled)
];

pub(crate) const BIOS_MODE3_GC_REGS: [u8; GC_REGS_INITIAL_LEN] = [
    0x00, // Set/Reset
    0x00, // Enable Set/Reset
    0x00, // Colour Compare
    0x00, // Data Rotate
    0x00, // Read Map Select
    0x10, // Mode (write mode 0, read mode 0)
    0x0E, // Misc (text, memory map = 0xB8000)
    0x00, // Colour Don't Care
    0xFF, // Bit Mask
];

pub(crate) const BIOS_MODE3_AC_REGS: [u8; AC_REGS_INITIAL_LEN] = [
    // Palette (0..=15)
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
    0x0E, 0x0F,
    0x0C, // Attribute Mode Control (text)
    0x00, // Overscan Colour
    0x0F, // Colour Plane Enable
    0x00, // Horizontal PEL Panning
    0x00, // Colour Select
];

pub(crate) const BIOS_MODE3_CRTC_REGS: [u8; CRTC_REGS_INITIAL_LEN] = [
    0x5F, // Horizontal Total
    0x4F, // Horizontal Display End
    0x50, // Start Horizontal Blanking
    0x82, // End Horizontal Blanking
    0x55, // Start Horizontal Retrace
    0x81, // End Horizontal Retrace
    0xBF, // Vertical Total
    0x1F, // Overflow
    0x00, // Preset Row Scan
    0x4F, // Maximum Scan Line
    0x0D, // Cursor Start
    0x0E, // Cursor End
    0x00, // Start Address High
    0x00, // Start Address Low
    0x00, // Cursor Location High
    0x00, // Cursor Location Low
    0x9C, // Vertical Retrace Start
    0x8E, // Vertical Retrace End
    0x8F, // Vertical Display End
    0x28, // Offset
    0x1F, // Underline Location
    0x96, // Start Vertical Blanking
    0xB9, // End Vertical Blanking
    0xA3, // CRTC Mode Control
    0xFF, // Line Compare
];
