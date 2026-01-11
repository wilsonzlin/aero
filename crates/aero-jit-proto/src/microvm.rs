use std::fmt;

pub type FuncId = usize;
pub type BlockId = usize;

/// Guest general-purpose register (u64).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gpr(pub u16);

impl fmt::Debug for Gpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// Guest SIMD register (u128, representing an SSE `xmm` register).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Xmm(pub u16);

impl fmt::Debug for Xmm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "xmm{}", self.0)
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct FlagMask: u8 {
        const ZF = 1 << 0;
        const SF = 1 << 1;
        const CF = 1 << 2;
        const OF = 1 << 3;
        const ALL = Self::ZF.bits() | Self::SF.bits() | Self::CF.bits() | Self::OF.bits();
    }
}

/// Branch condition evaluated against the current flags value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Zero,
    NonZero,
    Carry,
    NoCarry,
    Overflow,
    NoOverflow,
    Sign,
    NoSign,
}

impl Cond {
    pub fn uses_flags(self) -> FlagMask {
        match self {
            Cond::Zero | Cond::NonZero => FlagMask::ZF,
            Cond::Carry | Cond::NoCarry => FlagMask::CF,
            Cond::Overflow | Cond::NoOverflow => FlagMask::OF,
            Cond::Sign | Cond::NoSign => FlagMask::SF,
        }
    }

    pub fn eval(self, flags: u8) -> bool {
        let zf = (flags & FlagMask::ZF.bits()) != 0;
        let sf = (flags & FlagMask::SF.bits()) != 0;
        let cf = (flags & FlagMask::CF.bits()) != 0;
        let of = (flags & FlagMask::OF.bits()) != 0;
        match self {
            Cond::Zero => zf,
            Cond::NonZero => !zf,
            Cond::Carry => cf,
            Cond::NoCarry => !cf,
            Cond::Overflow => of,
            Cond::NoOverflow => !of,
            Cond::Sign => sf,
            Cond::NoSign => !sf,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Instr {
    /// `dst = imm`.
    Imm { dst: Gpr, imm: u64 },
    /// `dst = src`.
    Mov { dst: Gpr, src: Gpr },
    /// `dst = a + b` (sets flags).
    Add { dst: Gpr, a: Gpr, b: Gpr },
    /// `dst = a - b` (sets flags).
    Sub { dst: Gpr, a: Gpr, b: Gpr },
    /// `dst = a * b` (sets flags).
    Mul { dst: Gpr, a: Gpr, b: Gpr },
    /// `dst = src << shift` (sets flags).
    Shl { dst: Gpr, src: Gpr, shift: u8 },
    /// Compare `a - b` (sets flags, no gpr result).
    Cmp { a: Gpr, b: Gpr },

    /// `dst = load_u64(base + offset)`.
    Load { dst: Gpr, base: Gpr, offset: i32 },
    /// `store_u64(base + offset, src)`.
    Store { base: Gpr, offset: i32, src: Gpr },

    /// `dst = imm` (vector constant).
    VImm { dst: Xmm, imm: u128 },
    /// `dst = a + b` (lane-wise f32x4 add).
    VAddF32x4 { dst: Xmm, a: Xmm, b: Xmm },
    /// `dst = a * b` (lane-wise f32x4 mul).
    VMulF32x4 { dst: Xmm, a: Xmm, b: Xmm },

    /// Call another function, `dst = f(args...)`.
    Call {
        dst: Gpr,
        func: FuncId,
        args: Vec<Gpr>,
    },
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Jmp(BlockId),
    Br {
        cond: Cond,
        then_tgt: BlockId,
        else_tgt: BlockId,
    },
    Ret {
        src: Gpr,
    },
}

#[derive(Clone, Debug)]
pub struct Block {
    pub instrs: Vec<Instr>,
    pub term: Terminator,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    pub gpr_count: u16,
    pub xmm_count: u16,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub functions: Vec<Function>,
}

/// A tiny memory model that supports:
/// - page permission epochs (permission changes force deopt)
/// - self-modifying code epoch (writes to executable pages force deopt)
#[derive(Clone)]
pub struct Memory {
    data: Vec<u8>,
    page_exec: Vec<bool>,
    perm_epoch: u64,
    code_epoch: u64,
}

impl Memory {
    pub fn new(size: usize) -> Self {
        let pages = size.div_ceil(4096);
        Self {
            data: vec![0; size],
            // Default to non-executable pages. Tests explicitly toggle exec for
            // "code pages" to exercise Tier-2 deoptimization.
            page_exec: vec![false; pages],
            perm_epoch: 1,
            code_epoch: 1,
        }
    }

    pub fn perm_epoch(&self) -> u64 {
        self.perm_epoch
    }

    pub fn code_epoch(&self) -> u64 {
        self.code_epoch
    }

    pub fn set_page_executable(&mut self, page_idx: usize, exec: bool) {
        if let Some(slot) = self.page_exec.get_mut(page_idx) {
            if *slot != exec {
                *slot = exec;
                self.perm_epoch = self.perm_epoch.wrapping_add(1);
            }
        }
    }

    pub fn load_u64(&self, addr: u64) -> u64 {
        let addr = addr as usize;
        let bytes = &self.data[addr..addr + 8];
        u64::from_le_bytes(bytes.try_into().unwrap())
    }

    pub fn store_u64(&mut self, addr: u64, value: u64) {
        let addr_usize = addr as usize;
        self.data[addr_usize..addr_usize + 8].copy_from_slice(&value.to_le_bytes());

        let page_idx = addr_usize / 4096;
        if self.page_exec.get(page_idx).copied().unwrap_or(false) {
            self.code_epoch = self.code_epoch.wrapping_add(1);
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr()
    }
}

#[derive(Clone)]
pub struct Vm {
    pub gprs: Vec<u64>,
    pub xmms: Vec<u128>,
    pub flags: u8,
    pub mem: Memory,
}

impl Vm {
    pub fn new(gpr_count: u16, xmm_count: u16, mem_size: usize) -> Self {
        Self {
            gprs: vec![0; gpr_count as usize],
            xmms: vec![0; xmm_count as usize],
            flags: 0,
            mem: Memory::new(mem_size),
        }
    }
}

pub(crate) fn set_flags_add(dst: u64, a: u64, b: u64, mask: FlagMask, flags: &mut u8) {
    if mask.is_empty() {
        return;
    }
    let mut out = *flags;
    if mask.contains(FlagMask::ZF) {
        out = (out & !FlagMask::ZF.bits()) | (((dst == 0) as u8) * FlagMask::ZF.bits());
    }
    if mask.contains(FlagMask::SF) {
        out = (out & !FlagMask::SF.bits()) | ((((dst as i64) < 0) as u8) * FlagMask::SF.bits());
    }
    if mask.contains(FlagMask::CF) {
        out = (out & !FlagMask::CF.bits()) | (((dst < a) as u8) * FlagMask::CF.bits());
    }
    if mask.contains(FlagMask::OF) {
        let of = (((a ^ dst) & (b ^ dst)) >> 63) & 1;
        out = (out & !FlagMask::OF.bits()) | ((of as u8) * FlagMask::OF.bits());
    }
    *flags = out;
}

pub(crate) fn set_flags_sub(dst: u64, a: u64, b: u64, mask: FlagMask, flags: &mut u8) {
    if mask.is_empty() {
        return;
    }
    let mut out = *flags;
    if mask.contains(FlagMask::ZF) {
        out = (out & !FlagMask::ZF.bits()) | (((dst == 0) as u8) * FlagMask::ZF.bits());
    }
    if mask.contains(FlagMask::SF) {
        out = (out & !FlagMask::SF.bits()) | ((((dst as i64) < 0) as u8) * FlagMask::SF.bits());
    }
    if mask.contains(FlagMask::CF) {
        out = (out & !FlagMask::CF.bits()) | (((a < b) as u8) * FlagMask::CF.bits());
    }
    if mask.contains(FlagMask::OF) {
        let of = (((a ^ b) & (a ^ dst)) >> 63) & 1;
        out = (out & !FlagMask::OF.bits()) | ((of as u8) * FlagMask::OF.bits());
    }
    *flags = out;
}

pub(crate) fn set_flags_logic(dst: u64, mask: FlagMask, flags: &mut u8) {
    if mask.is_empty() {
        return;
    }
    let mut out = *flags;
    if mask.contains(FlagMask::ZF) {
        out = (out & !FlagMask::ZF.bits()) | (((dst == 0) as u8) * FlagMask::ZF.bits());
    }
    if mask.contains(FlagMask::SF) {
        out = (out & !FlagMask::SF.bits()) | ((((dst as i64) < 0) as u8) * FlagMask::SF.bits());
    }
    // For simplicity, CF/OF are cleared for logical ops.
    if mask.contains(FlagMask::CF) {
        out &= !FlagMask::CF.bits();
    }
    if mask.contains(FlagMask::OF) {
        out &= !FlagMask::OF.bits();
    }
    *flags = out;
}

pub(crate) fn simd_f32x4_add(a: u128, b: u128) -> u128 {
    let mut out = 0u128;
    for lane in 0..4 {
        let shift = lane * 32;
        let aa = f32::from_bits((a >> shift) as u32);
        let bb = f32::from_bits((b >> shift) as u32);
        let rr = (aa + bb).to_bits() as u128;
        out |= rr << shift;
    }
    out
}

pub(crate) fn simd_f32x4_mul(a: u128, b: u128) -> u128 {
    let mut out = 0u128;
    for lane in 0..4 {
        let shift = lane * 32;
        let aa = f32::from_bits((a >> shift) as u32);
        let bb = f32::from_bits((b >> shift) as u32);
        let rr = (aa * bb).to_bits() as u128;
        out |= rr << shift;
    }
    out
}
