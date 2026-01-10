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

