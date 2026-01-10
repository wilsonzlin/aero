pub(crate) mod alu;
pub mod atomics;
pub mod bitext;
pub mod decode;
pub mod sse;
pub mod sse2;
pub mod sse3;
pub mod sse41;
pub mod sse42;
pub mod ssse3;
pub mod string;
pub mod tier0;
pub mod win7_ext;
pub mod x87;

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::{CpuState, Exception};

#[derive(Clone, Debug)]
pub struct DecodedInst {
    pub len: usize,
    pub kind: InstKind,
}

#[derive(Clone, Debug)]
pub enum InstKind {
    String(string::DecodedStringInst),
    Atomics(atomics::DecodedAtomicInst),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    InvalidOpcode(u8),
    TruncatedInstruction,
    Exception(Exception),
}

pub fn exec<B: Bus>(cpu: &mut Cpu, bus: &mut B, inst: &DecodedInst) -> Result<(), ExecError> {
    match &inst.kind {
        InstKind::String(s) => string::exec_string(cpu, bus, s),
        InstKind::Atomics(a) => atomics::exec_atomics(cpu, bus, a),
    }
}

// -------------------------------------------------------------------------------------------------
// SIMD/SSE helpers (decoder-agnostic building blocks).
// -------------------------------------------------------------------------------------------------

/// XMM register selector.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum XmmReg {
    Xmm0 = 0,
    Xmm1 = 1,
    Xmm2 = 2,
    Xmm3 = 3,
    Xmm4 = 4,
    Xmm5 = 5,
    Xmm6 = 6,
    Xmm7 = 7,
    Xmm8 = 8,
    Xmm9 = 9,
    Xmm10 = 10,
    Xmm11 = 11,
    Xmm12 = 12,
    Xmm13 = 13,
    Xmm14 = 14,
    Xmm15 = 15,
}

impl XmmReg {
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// XMM register or memory operand.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum XmmOperand {
    Reg(XmmReg),
    Mem(u64),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RoundingMode {
    Nearest,
    Down,
    Up,
    TowardZero,
}

// MXCSR flags (sticky exception bits).
pub const MXCSR_IE: u32 = 1 << 0;
pub const MXCSR_DE: u32 = 1 << 1;
pub const MXCSR_ZE: u32 = 1 << 2;
pub const MXCSR_OE: u32 = 1 << 3;
pub const MXCSR_UE: u32 = 1 << 4;
pub const MXCSR_PE: u32 = 1 << 5;
pub const MXCSR_EXCEPTION_FLAGS_MASK: u32 =
    MXCSR_IE | MXCSR_DE | MXCSR_ZE | MXCSR_OE | MXCSR_UE | MXCSR_PE;

pub const MXCSR_RC_MASK: u32 = 0b11 << 13;

#[inline]
pub fn rounding_mode(mxcsr: u32) -> RoundingMode {
    match (mxcsr & MXCSR_RC_MASK) >> 13 {
        0 => RoundingMode::Nearest,
        1 => RoundingMode::Down,
        2 => RoundingMode::Up,
        3 => RoundingMode::TowardZero,
        _ => unreachable!(),
    }
}

#[inline]
pub fn set_rounding_mode(mxcsr: &mut u32, mode: RoundingMode) {
    let rc_bits = match mode {
        RoundingMode::Nearest => 0,
        RoundingMode::Down => 1,
        RoundingMode::Up => 2,
        RoundingMode::TowardZero => 3,
    } << 13;
    *mxcsr = (*mxcsr & !MXCSR_RC_MASK) | rc_bits;
}

#[inline]
pub fn clear_exception_flags(mxcsr: &mut u32) {
    *mxcsr &= !MXCSR_EXCEPTION_FLAGS_MASK;
}

#[inline]
pub(crate) fn xmm(cpu: &CpuState, reg: XmmReg) -> u128 {
    cpu.sse.xmm[reg.index()]
}

#[inline]
pub(crate) fn or_mxcsr_flags(cpu: &mut CpuState, flags: u32) {
    cpu.sse.mxcsr |= flags;
}

#[inline]
pub(crate) fn read_xmm_operand_128<B: Bus>(cpu: &CpuState, bus: &mut B, src: XmmOperand) -> u128 {
    match src {
        XmmOperand::Reg(r) => xmm(cpu, r),
        XmmOperand::Mem(addr) => bus.read_u128(addr),
    }
}

#[inline]
pub(crate) fn read_xmm_operand_u32<B: Bus>(cpu: &CpuState, bus: &mut B, src: XmmOperand) -> u32 {
    match src {
        XmmOperand::Reg(r) => xmm(cpu, r) as u32,
        XmmOperand::Mem(addr) => bus.read_u32(addr),
    }
}

#[inline]
pub(crate) fn read_xmm_operand_u64<B: Bus>(cpu: &CpuState, bus: &mut B, src: XmmOperand) -> u64 {
    match src {
        XmmOperand::Reg(r) => xmm(cpu, r) as u64,
        XmmOperand::Mem(addr) => bus.read_u64(addr),
    }
}

#[inline]
pub(crate) fn check_alignment(enforce: bool, addr: u64, align: u64) -> Result<(), Exception> {
    if enforce && (addr % align != 0) {
        return Err(Exception::gp0());
    }
    Ok(())
}

#[inline]
pub(crate) fn u128_set_low_u32_preserve(high: u128, low: u32) -> u128 {
    (high & !0xFFFF_FFFFu128) | (low as u128)
}

#[inline]
pub(crate) fn u128_set_low_u64_preserve(high: u128, low: u64) -> u128 {
    (high & !0xFFFF_FFFF_FFFF_FFFFu128) | (low as u128)
}

