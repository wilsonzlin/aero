use std::fmt;

use aero_types::{Flag, Gpr, Width};

pub const REG_COUNT: usize = 16;

/// Bitmask of flags used by the Tier-2 optimizer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FlagMask(u64);

impl FlagMask {
    pub const EMPTY: Self = Self(0);
    pub const CF: Self = Self(1u64 << (Flag::Cf.rflags_bit() as u32));
    pub const PF: Self = Self(1u64 << (Flag::Pf.rflags_bit() as u32));
    pub const AF: Self = Self(1u64 << (Flag::Af.rflags_bit() as u32));
    pub const ZF: Self = Self(1u64 << (Flag::Zf.rflags_bit() as u32));
    pub const SF: Self = Self(1u64 << (Flag::Sf.rflags_bit() as u32));
    pub const OF: Self = Self(1u64 << (Flag::Of.rflags_bit() as u32));
    pub const ALL: Self =
        Self(Self::CF.0 | Self::PF.0 | Self::AF.0 | Self::ZF.0 | Self::SF.0 | Self::OF.0);

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    #[inline]
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    #[inline]
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl fmt::Debug for FlagMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("FlagMask(EMPTY)");
        }
        f.write_str("FlagMask(")?;
        let mut first = true;
        if self.intersects(Self::CF) {
            if !first {
                f.write_str("|")?;
            }
            first = false;
            f.write_str("CF")?;
        }
        if self.intersects(Self::PF) {
            if !first {
                f.write_str("|")?;
            }
            first = false;
            f.write_str("PF")?;
        }
        if self.intersects(Self::AF) {
            if !first {
                f.write_str("|")?;
            }
            first = false;
            f.write_str("AF")?;
        }
        if self.intersects(Self::ZF) {
            if !first {
                f.write_str("|")?;
            }
            first = false;
            f.write_str("ZF")?;
        }
        if self.intersects(Self::SF) {
            if !first {
                f.write_str("|")?;
            }
            first = false;
            f.write_str("SF")?;
        }
        if self.intersects(Self::OF) {
            if !first {
                f.write_str("|")?;
            }
            f.write_str("OF")?;
        }
        f.write_str(")")
    }
}

impl std::ops::BitOr for FlagMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for FlagMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl From<Flag> for FlagMask {
    fn from(value: Flag) -> Self {
        Self(1u64 << (value.rflags_bit() as u32))
    }
}

/// Concrete values for flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlagValues {
    pub cf: bool,
    pub pf: bool,
    pub af: bool,
    pub zf: bool,
    pub sf: bool,
    pub of: bool,
}

impl FlagValues {
    pub fn get(&self, flag: Flag) -> bool {
        match flag {
            Flag::Cf => self.cf,
            Flag::Pf => self.pf,
            Flag::Af => self.af,
            Flag::Zf => self.zf,
            Flag::Sf => self.sf,
            Flag::Of => self.of,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

impl ValueId {
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

impl BlockId {
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operand {
    Value(ValueId),
    Const(u64),
}

impl Operand {
    #[inline]
    pub fn as_value(self) -> Option<ValueId> {
        match self {
            Self::Value(v) => Some(v),
            Self::Const(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Eq,
    LtU,
}

impl BinOp {
    pub const fn is_commutative(self) -> bool {
        matches!(
            self,
            Self::Add | Self::Mul | Self::And | Self::Or | Self::Xor | Self::Eq
        )
    }
}

/// Evaluate a [`BinOp`], returning `(result, flags)` computed using a simplified x86-like model.
pub fn eval_binop(op: BinOp, lhs: u64, rhs: u64) -> (u64, FlagValues) {
    fn parity_even(byte: u8) -> bool {
        (byte.count_ones() & 1) == 0
    }

    match op {
        BinOp::Add => {
            let (res, cf) = lhs.overflowing_add(rhs);
            let of = ((lhs ^ res) & (rhs ^ res) & (1u64 << 63)) != 0;
            let af = ((lhs ^ rhs ^ res) & 0x10) != 0;
            let pf = parity_even(res as u8);
            let flags = FlagValues {
                cf,
                pf,
                af,
                zf: res == 0,
                sf: (res >> 63) != 0,
                of,
            };
            (res, flags)
        }
        BinOp::Sub => {
            let (res, cf) = lhs.overflowing_sub(rhs);
            let of = ((lhs ^ rhs) & (lhs ^ res) & (1u64 << 63)) != 0;
            let af = ((lhs ^ rhs ^ res) & 0x10) != 0;
            let pf = parity_even(res as u8);
            let flags = FlagValues {
                cf,
                pf,
                af,
                zf: res == 0,
                sf: (res >> 63) != 0,
                of,
            };
            (res, flags)
        }
        BinOp::Mul => {
            let res = lhs.wrapping_mul(rhs);
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: (res >> 63) != 0,
                    of: false,
                },
            )
        }
        BinOp::And => {
            let res = lhs & rhs;
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: (res >> 63) != 0,
                    of: false,
                },
            )
        }
        BinOp::Or => {
            let res = lhs | rhs;
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: (res >> 63) != 0,
                    of: false,
                },
            )
        }
        BinOp::Xor => {
            let res = lhs ^ rhs;
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: (res >> 63) != 0,
                    of: false,
                },
            )
        }
        BinOp::Shl => {
            let sh = (rhs & 63) as u32;
            let res = lhs.wrapping_shl(sh);
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: (res >> 63) != 0,
                    of: false,
                },
            )
        }
        BinOp::Shr => {
            let sh = (rhs & 63) as u32;
            let res = lhs.wrapping_shr(sh);
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: (res >> 63) != 0,
                    of: false,
                },
            )
        }
        BinOp::Eq => {
            let res = (lhs == rhs) as u64;
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: false,
                    of: false,
                },
            )
        }
        BinOp::LtU => {
            let res = (lhs < rhs) as u64;
            (
                res,
                FlagValues {
                    cf: false,
                    pf: parity_even(res as u8),
                    af: false,
                    zf: res == 0,
                    sf: false,
                    of: false,
                },
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instr {
    Nop,

    Const {
        dst: ValueId,
        value: u64,
    },
    LoadReg {
        dst: ValueId,
        reg: Gpr,
    },
    StoreReg {
        reg: Gpr,
        src: Operand,
    },

    LoadFlag {
        dst: ValueId,
        flag: Flag,
    },
    SetFlags {
        mask: FlagMask,
        values: FlagValues,
    },

    BinOp {
        dst: ValueId,
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
        flags: FlagMask,
    },

    /// x86 address computation: `base + index * scale + disp`.
    Addr {
        dst: ValueId,
        base: Operand,
        index: Operand,
        scale: u8,
        disp: i64,
    },

    LoadMem {
        dst: ValueId,
        addr: Operand,
        width: Width,
    },
    StoreMem {
        addr: Operand,
        src: Operand,
        width: Width,
    },

    Guard {
        cond: Operand,
        expected: bool,
        exit_rip: u64,
    },

    GuardCodeVersion {
        page: u64,
        expected: u64,
        exit_rip: u64,
    },

    SideExit {
        exit_rip: u64,
    },
}

impl Instr {
    pub fn dst(&self) -> Option<ValueId> {
        match *self {
            Self::Const { dst, .. }
            | Self::LoadReg { dst, .. }
            | Self::LoadFlag { dst, .. }
            | Self::BinOp { dst, .. }
            | Self::Addr { dst, .. }
            | Self::LoadMem { dst, .. } => Some(dst),
            Self::Nop
            | Self::StoreReg { .. }
            | Self::StoreMem { .. }
            | Self::SetFlags { .. }
            | Self::Guard { .. }
            | Self::GuardCodeVersion { .. }
            | Self::SideExit { .. } => None,
        }
    }

    pub fn flags_written(&self) -> FlagMask {
        match *self {
            Self::BinOp { flags, .. } => flags,
            Self::SetFlags { mask, .. } => mask,
            _ => FlagMask::EMPTY,
        }
    }

    pub fn flags_read(&self) -> FlagMask {
        match *self {
            Self::LoadFlag { flag, .. } => flag.into(),
            _ => FlagMask::EMPTY,
        }
    }

    pub fn has_side_effects(&self) -> bool {
        match self {
            Self::StoreReg { .. }
            | Self::LoadMem { .. }
            | Self::StoreMem { .. }
            | Self::Guard { .. }
            | Self::GuardCodeVersion { .. }
            | Self::SideExit { .. }
            | Self::SetFlags { .. } => true,
            Self::BinOp { flags, .. } => !flags.is_empty(),
            Self::Nop
            | Self::Const { .. }
            | Self::LoadReg { .. }
            | Self::LoadFlag { .. }
            | Self::Addr { .. } => false,
        }
    }

    pub fn is_terminator(&self) -> bool {
        matches!(self, Self::SideExit { .. })
    }

    pub fn for_each_operand(&self, mut f: impl FnMut(Operand)) {
        match *self {
            Self::BinOp { lhs, rhs, .. } => {
                f(lhs);
                f(rhs);
            }
            Self::StoreReg { src, .. } => f(src),
            Self::Addr { base, index, .. } => {
                f(base);
                f(index);
            }
            Self::LoadMem { addr, .. } => f(addr),
            Self::StoreMem { addr, src, .. } => {
                f(addr);
                f(src);
            }
            Self::Guard { cond, .. } => f(cond),
            Self::Nop
            | Self::Const { .. }
            | Self::LoadReg { .. }
            | Self::LoadFlag { .. }
            | Self::SetFlags { .. }
            | Self::GuardCodeVersion { .. }
            | Self::SideExit { .. } => {}
        }
    }

    pub fn for_each_operand_mut(&mut self, mut f: impl FnMut(&mut Operand)) {
        match self {
            Self::BinOp { lhs, rhs, .. } => {
                f(lhs);
                f(rhs);
            }
            Self::StoreReg { src, .. } => f(src),
            Self::Addr { base, index, .. } => {
                f(base);
                f(index);
            }
            Self::LoadMem { addr, .. } => f(addr),
            Self::StoreMem { addr, src, .. } => {
                f(addr);
                f(src);
            }
            Self::Guard { cond, .. } => f(cond),
            Self::Nop
            | Self::Const { .. }
            | Self::LoadReg { .. }
            | Self::LoadFlag { .. }
            | Self::SetFlags { .. }
            | Self::GuardCodeVersion { .. }
            | Self::SideExit { .. } => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    Linear,
    Loop,
}

impl Default for TraceKind {
    fn default() -> Self {
        Self::Linear
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceIr {
    pub prologue: Vec<Instr>,
    pub body: Vec<Instr>,
    pub kind: TraceKind,
}

impl TraceIr {
    pub fn iter_instrs(&self) -> impl Iterator<Item = &Instr> {
        self.prologue.iter().chain(self.body.iter())
    }

    pub fn iter_instrs_mut(&mut self) -> impl Iterator<Item = &mut Instr> {
        let (prologue, body) = (&mut self.prologue, &mut self.body);
        prologue.iter_mut().chain(body.iter_mut())
    }

    pub fn body_regs_written(&self) -> [bool; REG_COUNT] {
        let mut written = [false; REG_COUNT];
        for inst in &self.body {
            if let Instr::StoreReg { reg, .. } = *inst {
                written[reg.as_u8() as usize] = true;
            }
        }
        written
    }
}

#[derive(Clone, Debug)]
pub struct Function {
    pub blocks: Vec<Block>,
    pub entry: BlockId,
}

impl Function {
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.index()]
    }

    pub fn find_block_by_rip(&self, rip: u64) -> Option<BlockId> {
        self.blocks
            .iter()
            .find(|b| b.start_rip == rip)
            .map(|b| b.id)
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub id: BlockId,
    pub start_rip: u64,
    pub instrs: Vec<Instr>,
    pub term: Terminator,
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Jump(BlockId),
    Branch {
        cond: Operand,
        then_bb: BlockId,
        else_bb: BlockId,
    },
    SideExit {
        exit_rip: u64,
    },
    Return,
}
