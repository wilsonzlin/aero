#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct XmmReg(u8);

impl XmmReg {
    pub const fn new(idx: u8) -> Option<Self> {
        if idx < 16 { Some(Self(idx)) } else { None }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub const fn state_byte_offset(self) -> u32 {
        (self.0 as u32) * 16
    }
}

impl TryFrom<u8> for XmmReg {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Reg(XmmReg),
    /// Absolute guest memory address (byte address), relative to the guest memory base.
    Mem(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inst {
    /// `MOVDQU xmm, m128`
    MovdquLoad { dst: XmmReg, addr: u32 },
    /// `MOVDQU m128, xmm`
    MovdquStore { addr: u32, src: XmmReg },

    /// `ADDPS xmm, xmm/m128`
    Addps { dst: XmmReg, src: Operand },
    /// `SUBPS xmm, xmm/m128`
    Subps { dst: XmmReg, src: Operand },
    /// `MULPS xmm, xmm/m128`
    Mulps { dst: XmmReg, src: Operand },
    /// `DIVPS xmm, xmm/m128`
    Divps { dst: XmmReg, src: Operand },

    /// `ADDPD xmm, xmm/m128`
    Addpd { dst: XmmReg, src: Operand },
    /// `SUBPD xmm, xmm/m128`
    Subpd { dst: XmmReg, src: Operand },
    /// `MULPD xmm, xmm/m128`
    Mulpd { dst: XmmReg, src: Operand },
    /// `DIVPD xmm, xmm/m128`
    Divpd { dst: XmmReg, src: Operand },

    /// `PAND xmm, xmm/m128`
    Pand { dst: XmmReg, src: Operand },
    /// `POR xmm, xmm/m128`
    Por { dst: XmmReg, src: Operand },
    /// `PXOR xmm, xmm/m128`
    Pxor { dst: XmmReg, src: Operand },

    /// `PSHUFB xmm, xmm/m128` (SSSE3)
    Pshufb { dst: XmmReg, src: Operand },

    /// `SQRTPS xmm, xmm/m128`
    Sqrtps { dst: XmmReg, src: Operand },
    /// `SQRTPD xmm, xmm/m128`
    Sqrtpd { dst: XmmReg, src: Operand },

    /// `PSLLD xmm, xmm/m128` (SSE2)
    Pslld { dst: XmmReg, src: Operand },
    /// `PSRLD xmm, xmm/m128` (SSE2)
    Psrld { dst: XmmReg, src: Operand },

    /// `PSLLW xmm, xmm/m128` (SSE2)
    Psllw { dst: XmmReg, src: Operand },
    /// `PSRLW xmm, xmm/m128` (SSE2)
    Psrlw { dst: XmmReg, src: Operand },

    /// `PSLLQ xmm, xmm/m128` (SSE2)
    Psllq { dst: XmmReg, src: Operand },
    /// `PSRLQ xmm, xmm/m128` (SSE2)
    Psrlq { dst: XmmReg, src: Operand },

    /// `PSLLD xmm, imm8` (SSE2)
    PslldImm { dst: XmmReg, imm: u8 },
    /// `PSRLD xmm, imm8` (SSE2)
    PsrldImm { dst: XmmReg, imm: u8 },

    /// `PSLLW xmm, imm8` (SSE2)
    PsllwImm { dst: XmmReg, imm: u8 },
    /// `PSRLW xmm, imm8` (SSE2)
    PsrlwImm { dst: XmmReg, imm: u8 },

    /// `PSLLQ xmm, imm8` (SSE2)
    PsllqImm { dst: XmmReg, imm: u8 },
    /// `PSRLQ xmm, imm8` (SSE2)
    PsrlqImm { dst: XmmReg, imm: u8 },

    /// `PSLLDQ xmm, imm8` (SSE2)
    PslldqImm { dst: XmmReg, imm: u8 },
    /// `PSRLDQ xmm, imm8` (SSE2)
    PsrldqImm { dst: XmmReg, imm: u8 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    pub insts: Vec<Inst>,
}
