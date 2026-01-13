use aero_types::{Flag, FlagSet, Gpr, Width};
use std::collections::HashMap;

pub const REG_COUNT: usize = 16;

pub const ALL_GPRS: [Gpr; REG_COUNT] = [
    Gpr::Rax,
    Gpr::Rcx,
    Gpr::Rdx,
    Gpr::Rbx,
    Gpr::Rsp,
    Gpr::Rbp,
    Gpr::Rsi,
    Gpr::Rdi,
    Gpr::R8,
    Gpr::R9,
    Gpr::R10,
    Gpr::R11,
    Gpr::R12,
    Gpr::R13,
    Gpr::R14,
    Gpr::R15,
];

#[inline]
pub const fn flag_to_set(flag: Flag) -> FlagSet {
    match flag {
        Flag::Cf => FlagSet::CF,
        Flag::Pf => FlagSet::PF,
        Flag::Af => FlagSet::AF,
        Flag::Zf => FlagSet::ZF,
        Flag::Sf => FlagSet::SF,
        Flag::Of => FlagSet::OF,
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
    Sar,
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
        byte.count_ones().is_multiple_of(2)
    }

    match op {
        BinOp::Add => {
            let (res, cf) = lhs.overflowing_add(rhs);
            let of = ((lhs ^ res) & (rhs ^ res) & (1u64 << 63)) != 0;
            let af = ((lhs ^ rhs ^ res) & 0x10) != 0;
            let flags = FlagValues {
                cf,
                pf: parity_even(res as u8),
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
            let flags = FlagValues {
                cf,
                pf: parity_even(res as u8),
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
        BinOp::Sar => {
            let sh = (rhs & 63) as u32;
            let res = ((lhs as i64).wrapping_shr(sh)) as u64;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    LoadFlag {
        dst: ValueId,
        flag: Flag,
    },
    SetFlags {
        mask: FlagSet,
        values: FlagValues,
    },

    BinOp {
        dst: ValueId,
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
        flags: FlagSet,
    },

    /// x86 address computation: `base + index * scale + disp`.
    Addr {
        dst: ValueId,
        base: Operand,
        index: Operand,
        scale: u8,
        disp: i64,
    },

    Guard {
        cond: Operand,
        expected: bool,
        exit_rip: u64,
    },

    GuardCodeVersion {
        page: u64,
        expected: u32,
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
            | Self::LoadMem { dst, .. }
            | Self::LoadFlag { dst, .. }
            | Self::BinOp { dst, .. }
            | Self::Addr { dst, .. } => Some(dst),
            Self::Nop
            | Self::StoreReg { .. }
            | Self::StoreMem { .. }
            | Self::SetFlags { .. }
            | Self::Guard { .. }
            | Self::GuardCodeVersion { .. }
            | Self::SideExit { .. } => None,
        }
    }

    pub fn flags_written(&self) -> FlagSet {
        match *self {
            Self::BinOp { flags, .. } => flags,
            Self::SetFlags { mask, .. } => mask,
            _ => FlagSet::EMPTY,
        }
    }

    pub fn flags_read(&self) -> FlagSet {
        match *self {
            Self::LoadFlag { flag, .. } => flag_to_set(flag),
            _ => FlagSet::EMPTY,
        }
    }

    pub fn has_side_effects(&self) -> bool {
        match self {
            Self::StoreReg { .. }
            | Self::LoadMem { .. }
            | Self::StoreMem { .. }
            | Self::Guard { .. }
            | Self::GuardCodeVersion { .. }
            | Self::SideExit { .. } => true,
            Self::SetFlags { mask, .. } => !mask.is_empty(),
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
            Self::LoadMem { addr, .. } => f(addr),
            Self::StoreMem { addr, src, .. } => {
                f(addr);
                f(src);
            }
            Self::Addr { base, index, .. } => {
                f(base);
                f(index);
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
            Self::LoadMem { addr, .. } => f(addr),
            Self::StoreMem { addr, src, .. } => {
                f(addr);
                f(src);
            }
            Self::Addr { base, index, .. } => {
                f(base);
                f(index);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TraceKind {
    #[default]
    Linear,
    Loop,
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

    /// Validate Tier-2 trace IR invariants.
    ///
    /// This is intended as a debug-only verifier; callers should typically gate it behind
    /// `debug_assertions` (e.g. via `debug_assert!(trace.validate().is_ok())`).
    pub fn validate(&self) -> Result<(), String> {
        if self.kind == TraceKind::Loop && self.body.is_empty() {
            return Err("TraceKind::Loop must have a non-empty body".to_owned());
        }

        // `Instr::SideExit` is an unconditional terminator. Having any instructions after it is
        // always a bug because they will never execute.
        if let Some(pos) = self
            .prologue
            .iter()
            .position(|i| matches!(i, Instr::SideExit { .. }))
        {
            if pos + 1 != self.prologue.len() {
                return Err(format!(
                    "Instr::SideExit must be the final instruction of the trace prologue: found at prologue[{pos}] but prologue has {} instructions",
                    self.prologue.len()
                ));
            }
            if !self.body.is_empty() {
                return Err(format!(
                    "Instr::SideExit in the prologue terminates the trace, but the trace body is non-empty ({} instructions)",
                    self.body.len()
                ));
            }
        }

        if let Some(pos) = self
            .body
            .iter()
            .position(|i| matches!(i, Instr::SideExit { .. }))
        {
            if self.kind == TraceKind::Linear && pos + 1 != self.body.len() {
                return Err(format!(
                    "Instr::SideExit is a terminator and must be the final instruction of a linear trace body: found at body[{pos}] but body has {} instructions",
                    self.body.len()
                ));
            }
            if pos + 1 != self.body.len() {
                return Err(format!(
                    "Instr::SideExit must be the final instruction of the trace body: found at body[{pos}] but body has {} instructions",
                    self.body.len()
                ));
            }
        }

        let prologue_len = self.prologue.len();
        let fmt_loc = |i: usize| -> String {
            if i < prologue_len {
                format!("prologue[{i}]")
            } else {
                format!("body[{}]", i - prologue_len)
            }
        };

        // Collect all SSA definitions and compute a "max_value_id" based solely on definitions.
        // This lets us cheaply catch operands that refer to completely out-of-range ValueIds
        // without allocating huge vectors (which many passes do based on max_id).
        let mut defs: HashMap<ValueId, usize> = HashMap::new();
        let mut max_value_id: u32 = 0;

        for (idx, inst) in self.iter_instrs().enumerate() {
            let Some(dst) = inst.dst() else { continue };
            if let Some(prev) = defs.insert(dst, idx) {
                return Err(format!(
                    "ValueId {dst:?} is defined multiple times (at {} and {})",
                    fmt_loc(prev),
                    fmt_loc(idx)
                ));
            }
            let next = dst
                .0
                .checked_add(1)
                .ok_or_else(|| format!("ValueId {dst:?} overflows max_value_id"))?;
            max_value_id = max_value_id.max(next);
        }

        // Validate all uses: bounds check, SSA resolution, and use-before-def.
        for (idx, inst) in self.iter_instrs().enumerate() {
            let mut err: Option<String> = None;
            inst.for_each_operand(|op| {
                if err.is_some() {
                    return;
                }
                let Operand::Value(v) = op else { return };

                if v.0 >= max_value_id {
                    err = Some(format!(
                        "use of {v:?} at {} exceeds max_value_id ({max_value_id})",
                        fmt_loc(idx)
                    ));
                    return;
                }

                let Some(&def_idx) = defs.get(&v) else {
                    err = Some(format!(
                        "use of undefined {v:?} at {} (no defining instruction)",
                        fmt_loc(idx)
                    ));
                    return;
                };

                if def_idx >= idx {
                    err = Some(format!(
                        "use-before-def of {v:?} at {} (defined at {})",
                        fmt_loc(idx),
                        fmt_loc(def_idx)
                    ));
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
        }

        Ok(())
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
    /// Length of the guest machine-code region covered by this block (in bytes).
    ///
    /// This is used to compute which 4KiB pages must be guarded against self-modifying code when
    /// building Tier-2 traces.
    pub code_len: u32,
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
