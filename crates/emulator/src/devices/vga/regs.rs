#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VgaPlanarShift {
    /// Standard planar shift (text mode / planar graphics).
    #[default]
    None,
    /// 256-colour shift (packed/chain4-style).
    Shift256,
    /// Interleaved shift (packed pixels across planes).
    Interleaved,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VgaDerivedState {
    /// Heuristic: does the current register set describe a graphics mode?
    pub is_graphics: bool,
    /// VGA chain-4 addressing (SEQ Memory Mode bit 3).
    pub chain4: bool,
    /// Odd/even addressing enabled (SEQ Memory Mode bit 2 and/or GC Mode bit 4).
    pub odd_even: bool,
    pub planar_shift: VgaPlanarShift,
    /// Best-effort guess; not authoritative.
    pub bpp_guess: u8,
    /// CRTC start address (CRTC[0x0C]:[0x0D]).
    pub start_address: u32,
    /// Bytes per scanline derived from CRTC offset/mode bits.
    pub pitch_bytes: u32,
    /// VGA memory window base as selected by GC[0x06] memory map bits.
    pub vram_window_base: u32,
    /// VGA memory window size as selected by GC[0x06] memory map bits.
    pub vram_window_size: u32,
    /// Best-effort width/height for common BIOS modes.
    pub width: u32,
    pub height: u32,
    /// Text dimensions when `is_graphics` is false.
    pub text_columns: u16,
    pub text_rows: u16,
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
