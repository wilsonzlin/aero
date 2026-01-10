/// x87 / MMX architectural state sufficient for `FXSAVE`/`FXRSTOR`.
///
/// Note: in the `FXSAVE` area each x87 register occupies a 16-byte slot, but
/// only the low 10 bytes are architecturally meaningful (80-bit extended
/// precision). The upper 6 bytes are reserved and are treated as zero on save
/// and ignored on restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpuState {
    /// FPU Control Word.
    pub fcw: u16,
    /// FPU Status Word.
    ///
    /// Note: the TOP-of-stack bits (11..=13) are stored separately in [`Self::top`].
    pub fsw: u16,
    /// FPU Tag Word in abridged (8-bit) form as stored by `FXSAVE`.
    pub ftw: u8,
    /// Logical top-of-stack (0-7). Mirrors bits 11..13 of `fsw`.
    pub top: u8,
    /// FPU last instruction opcode.
    pub fop: u16,
    /// FPU instruction pointer (legacy: offset, 64-bit: RIP).
    pub fip: u64,
    /// FPU data pointer (legacy: offset, 64-bit: RDP).
    pub fdp: u64,
    /// Legacy: FPU instruction pointer CS selector.
    pub fcs: u16,
    /// Legacy: FPU data pointer DS selector.
    pub fds: u16,
    /// ST0..ST7 register image (each element is the 16-byte slot in the FXSAVE
    /// area: 80-bit value + 6 reserved bytes).
    pub st: [u128; 8],
}

/// Mask for the architecturally meaningful 80-bit portion of an x87 register
/// slot as stored in the FXSAVE area.
pub const ST80_MASK: u128 = (1u128 << 80) - 1;

pub fn canonicalize_st(raw: u128) -> u128 {
    raw & ST80_MASK
}

impl Default for FpuState {
    fn default() -> Self {
        let mut state = Self {
            fcw: 0,
            fsw: 0,
            ftw: 0,
            top: 0,
            fop: 0,
            fip: 0,
            fdp: 0,
            fcs: 0,
            fds: 0,
            st: [0u128; 8],
        };
        state.reset();
        state
    }
}

impl FpuState {
    pub fn reset(&mut self) {
        // SDM: After FINIT/FNINIT.
        self.fcw = 0x037F;
        self.fsw = 0;
        self.top = 0;
        self.ftw = 0; // abridged = all empty.
        self.fop = 0;
        self.fip = 0;
        self.fdp = 0;
        self.fcs = 0;
        self.fds = 0;
        self.st = [0u128; 8];
    }

    pub fn emms(&mut self) {
        self.ftw = 0;
    }

    pub fn fsw_with_top(&self) -> u16 {
        let mut fsw = self.fsw & !(0b111 << 11);
        fsw |= ((self.top as u16) & 0b111) << 11;
        fsw
    }
}
