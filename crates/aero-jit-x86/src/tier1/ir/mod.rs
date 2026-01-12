use aero_types::{Cond, FlagSet, Gpr, Width};
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId(pub u32);

impl fmt::Display for ValueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Xor => "xor",
            BinOp::Shl => "shl",
            BinOp::Shr => "shr",
            BinOp::Sar => "sar",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestReg {
    Rip,
    Gpr { reg: Gpr, width: Width, high8: bool },
    Flag(aero_types::Flag),
}

impl fmt::Display for GuestReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuestReg::Rip => f.write_str("rip"),
            GuestReg::Gpr { reg, width, high8 } => {
                // Mirror the `aero_x86::Reg` formatting for stability.
                if *width == Width::W8 {
                    if *high8 {
                        let s = match reg {
                            Gpr::Rax => "ah",
                            Gpr::Rcx => "ch",
                            Gpr::Rdx => "dh",
                            Gpr::Rbx => "bh",
                            _ => "??",
                        };
                        return f.write_str(s);
                    }
                    let s = match reg {
                        Gpr::Rax => "al",
                        Gpr::Rcx => "cl",
                        Gpr::Rdx => "dl",
                        Gpr::Rbx => "bl",
                        Gpr::Rsp => "spl",
                        Gpr::Rbp => "bpl",
                        Gpr::Rsi => "sil",
                        Gpr::Rdi => "dil",
                        Gpr::R8 => "r8b",
                        Gpr::R9 => "r9b",
                        Gpr::R10 => "r10b",
                        Gpr::R11 => "r11b",
                        Gpr::R12 => "r12b",
                        Gpr::R13 => "r13b",
                        Gpr::R14 => "r14b",
                        Gpr::R15 => "r15b",
                    };
                    return f.write_str(s);
                }
                if *width == Width::W16 {
                    let s = match reg {
                        Gpr::Rax => "ax",
                        Gpr::Rcx => "cx",
                        Gpr::Rdx => "dx",
                        Gpr::Rbx => "bx",
                        Gpr::Rsp => "sp",
                        Gpr::Rbp => "bp",
                        Gpr::Rsi => "si",
                        Gpr::Rdi => "di",
                        Gpr::R8 => "r8w",
                        Gpr::R9 => "r9w",
                        Gpr::R10 => "r10w",
                        Gpr::R11 => "r11w",
                        Gpr::R12 => "r12w",
                        Gpr::R13 => "r13w",
                        Gpr::R14 => "r14w",
                        Gpr::R15 => "r15w",
                    };
                    return f.write_str(s);
                }
                if *width == Width::W32 {
                    let s = match reg {
                        Gpr::Rax => "eax",
                        Gpr::Rcx => "ecx",
                        Gpr::Rdx => "edx",
                        Gpr::Rbx => "ebx",
                        Gpr::Rsp => "esp",
                        Gpr::Rbp => "ebp",
                        Gpr::Rsi => "esi",
                        Gpr::Rdi => "edi",
                        Gpr::R8 => "r8d",
                        Gpr::R9 => "r9d",
                        Gpr::R10 => "r10d",
                        Gpr::R11 => "r11d",
                        Gpr::R12 => "r12d",
                        Gpr::R13 => "r13d",
                        Gpr::R14 => "r14d",
                        Gpr::R15 => "r15d",
                    };
                    return f.write_str(s);
                }
                write!(f, "{reg}")
            }
            GuestReg::Flag(flag) => write!(f, "{flag}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideEffects(u32);

impl SideEffects {
    pub const NONE: SideEffects = SideEffects(0);
    pub const READ_MEM: SideEffects = SideEffects(1 << 0);
    pub const WRITE_MEM: SideEffects = SideEffects(1 << 1);
    pub const READ_REG: SideEffects = SideEffects(1 << 2);
    pub const WRITE_REG: SideEffects = SideEffects(1 << 3);
    pub const READ_FLAG: SideEffects = SideEffects(1 << 4);
    pub const WRITE_FLAG: SideEffects = SideEffects(1 << 5);
    pub const CALL_HELPER: SideEffects = SideEffects(1 << 6);

    #[must_use]
    pub const fn union(self, other: SideEffects) -> SideEffects {
        SideEffects(self.0 | other.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrInst {
    Const {
        dst: ValueId,
        value: u64,
        width: Width,
    },
    ReadReg {
        dst: ValueId,
        reg: GuestReg,
    },
    WriteReg {
        reg: GuestReg,
        src: ValueId,
    },
    Trunc {
        dst: ValueId,
        src: ValueId,
        width: Width,
    },
    Load {
        dst: ValueId,
        addr: ValueId,
        width: Width,
    },
    Store {
        addr: ValueId,
        src: ValueId,
        width: Width,
    },
    BinOp {
        dst: ValueId,
        op: BinOp,
        lhs: ValueId,
        rhs: ValueId,
        width: Width,
        flags: FlagSet,
    },
    CmpFlags {
        lhs: ValueId,
        rhs: ValueId,
        width: Width,
        flags: FlagSet,
    },
    TestFlags {
        lhs: ValueId,
        rhs: ValueId,
        width: Width,
        flags: FlagSet,
    },
    EvalCond {
        dst: ValueId,
        cond: Cond,
    },
    Select {
        dst: ValueId,
        cond: ValueId,
        if_true: ValueId,
        if_false: ValueId,
        width: Width,
    },
    CallHelper {
        helper: &'static str,
        args: Vec<ValueId>,
        ret: Option<(ValueId, Width)>,
    },
}

impl IrInst {
    #[must_use]
    pub fn side_effects(&self) -> SideEffects {
        match self {
            IrInst::Const { .. } => SideEffects::NONE,
            IrInst::ReadReg { reg, .. } => match reg {
                GuestReg::Flag(_) => SideEffects::READ_FLAG,
                _ => SideEffects::READ_REG,
            },
            IrInst::WriteReg { reg, .. } => match reg {
                GuestReg::Flag(_) => SideEffects::WRITE_FLAG,
                _ => SideEffects::WRITE_REG,
            },
            IrInst::Trunc { .. } => SideEffects::NONE,
            IrInst::Load { .. } => SideEffects::READ_MEM,
            IrInst::Store { .. } => SideEffects::WRITE_MEM,
            IrInst::BinOp { flags, .. } => {
                if flags.is_empty() {
                    SideEffects::NONE
                } else {
                    SideEffects::WRITE_FLAG
                }
            }
            IrInst::CmpFlags { flags, .. } | IrInst::TestFlags { flags, .. } => {
                if flags.is_empty() {
                    SideEffects::NONE
                } else {
                    SideEffects::WRITE_FLAG
                }
            }
            IrInst::EvalCond { cond, .. } => {
                if cond.uses_flags().is_empty() {
                    SideEffects::NONE
                } else {
                    SideEffects::READ_FLAG
                }
            }
            IrInst::Select { .. } => SideEffects::NONE,
            IrInst::CallHelper { .. } => SideEffects::CALL_HELPER,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrTerminator {
    Jump {
        target: u64,
    },
    CondJump {
        cond: ValueId,
        target: u64,
        fallthrough: u64,
    },
    IndirectJump {
        target: ValueId,
    },
    ExitToInterpreter {
        next_rip: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrBlock {
    pub entry_rip: u64,
    pub insts: Vec<IrInst>,
    pub terminator: IrTerminator,
    pub value_types: Vec<Width>,
}

impl IrBlock {
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("block 0x{:x}:\n", self.entry_rip));
        for inst in &self.insts {
            out.push_str("  ");
            out.push_str(&inst_to_text(inst));
            out.push('\n');
        }
        out.push_str("  term ");
        out.push_str(&term_to_text(&self.terminator));
        out.push('\n');
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        let types = &self.value_types;
        let mut defined = vec![false; types.len()];

        let mut define = |dst: ValueId, width: Width| -> Result<(), String> {
            let idx = dst.0 as usize;
            if idx >= types.len() {
                return Err(format!("value {dst} out of range"));
            }
            if defined[idx] {
                return Err(format!("value {dst} defined twice"));
            }
            if types[idx] != width {
                return Err(format!(
                    "value {dst} has declared type {}, but instruction defines {}",
                    types[idx], width
                ));
            }
            defined[idx] = true;
            Ok(())
        };

        let use_val = |v: ValueId| -> Result<Width, String> {
            let idx = v.0 as usize;
            types
                .get(idx)
                .copied()
                .ok_or_else(|| format!("use of out-of-range value {v}"))
        };

        for inst in &self.insts {
            match *inst {
                IrInst::Const { dst, width, .. } => define(dst, width)?,
                IrInst::ReadReg {
                    dst,
                    reg: GuestReg::Gpr { width, .. },
                } => define(dst, width)?,
                IrInst::ReadReg {
                    dst,
                    reg: GuestReg::Rip,
                } => define(dst, Width::W64)?,
                IrInst::ReadReg {
                    dst,
                    reg: GuestReg::Flag(_),
                } => define(dst, Width::W8)?,
                IrInst::Trunc { dst, src, width } => {
                    let src_ty = use_val(src)?;
                    if width.bits() > src_ty.bits() {
                        return Err(format!("Trunc from {src_ty} to {width} increases width"));
                    }
                    define(dst, width)?;
                }
                IrInst::WriteReg {
                    reg: GuestReg::Gpr { width, .. },
                    src,
                } => {
                    let src_ty = use_val(src)?;
                    if src_ty != width {
                        return Err(format!("WriteReg width mismatch: {src_ty} -> {width}"));
                    }
                }
                IrInst::WriteReg {
                    reg: GuestReg::Rip,
                    src,
                } => {
                    let src_ty = use_val(src)?;
                    if src_ty != Width::W64 {
                        return Err(format!("WriteReg RIP expects i64, got {src_ty}"));
                    }
                }
                IrInst::WriteReg {
                    reg: GuestReg::Flag(_),
                    src,
                } => {
                    let src_ty = use_val(src)?;
                    if src_ty != Width::W8 {
                        return Err(format!("WriteReg flag expects i8, got {src_ty}"));
                    }
                }
                IrInst::Load { dst, addr, width } => {
                    if use_val(addr)? != Width::W64 {
                        return Err("Load addr must be i64".to_string());
                    }
                    define(dst, width)?;
                }
                IrInst::Store { addr, src, width } => {
                    if use_val(addr)? != Width::W64 {
                        return Err("Store addr must be i64".to_string());
                    }
                    if use_val(src)? != width {
                        return Err("Store width mismatch".to_string());
                    }
                }
                IrInst::BinOp {
                    dst,
                    lhs,
                    rhs,
                    width,
                    ..
                } => {
                    if use_val(lhs)? != width || use_val(rhs)? != width {
                        return Err("BinOp width mismatch".to_string());
                    }
                    define(dst, width)?;
                }
                IrInst::CmpFlags {
                    lhs, rhs, width, ..
                }
                | IrInst::TestFlags {
                    lhs, rhs, width, ..
                } => {
                    if use_val(lhs)? != width || use_val(rhs)? != width {
                        return Err("Flag op width mismatch".to_string());
                    }
                }
                IrInst::EvalCond { dst, .. } => define(dst, Width::W8)?,
                IrInst::Select {
                    dst,
                    cond,
                    if_true,
                    if_false,
                    width,
                } => {
                    if use_val(cond)? != Width::W8 {
                        return Err("Select cond must be i8".to_string());
                    }
                    if use_val(if_true)? != width || use_val(if_false)? != width {
                        return Err("Select arm width mismatch".to_string());
                    }
                    define(dst, width)?;
                }
                IrInst::CallHelper { ret, ref args, .. } => {
                    for &arg in args {
                        let _ = use_val(arg)?;
                    }
                    if let Some((dst, width)) = ret {
                        define(dst, width)?;
                    }
                }
            }
        }

        match self.terminator {
            IrTerminator::Jump { .. } => {}
            IrTerminator::CondJump { cond, .. } => {
                if use_val(cond)? != Width::W8 {
                    return Err("CondJump cond must be i8".to_string());
                }
            }
            IrTerminator::IndirectJump { target } => {
                let ty = use_val(target)?;
                if !matches!(ty, Width::W32 | Width::W64) {
                    return Err(format!("IndirectJump target must be i32/i64, got {ty}"));
                }
            }
            IrTerminator::ExitToInterpreter { .. } => {}
        }

        Ok(())
    }
}

fn inst_to_text(inst: &IrInst) -> String {
    match inst {
        IrInst::Const { dst, value, width } => format!("{dst} = const.{width} 0x{value:x}"),
        IrInst::ReadReg { dst, reg } => format!("{dst} = read.{reg}"),
        IrInst::WriteReg { reg, src } => format!("write.{reg} {src}"),
        IrInst::Trunc { dst, src, width } => format!("{dst} = trunc.{width} {src}"),
        IrInst::Load { dst, addr, width } => format!("{dst} = load.{width} [{addr}]"),
        IrInst::Store { addr, src, width } => format!("store.{width} [{addr}], {src}"),
        IrInst::BinOp {
            dst,
            op,
            lhs,
            rhs,
            width,
            flags,
        } => {
            if flags.is_empty() {
                format!("{dst} = {op}.{width} {lhs}, {rhs}")
            } else {
                format!("{dst} = {op}.{width} {lhs}, {rhs} ; flags={flags}")
            }
        }
        IrInst::CmpFlags {
            lhs,
            rhs,
            width,
            flags,
        } => {
            if flags.is_empty() {
                format!("cmpflags.{width} {lhs}, {rhs}")
            } else {
                format!("cmpflags.{width} {lhs}, {rhs} ; flags={flags}")
            }
        }
        IrInst::TestFlags {
            lhs,
            rhs,
            width,
            flags,
        } => {
            if flags.is_empty() {
                format!("testflags.{width} {lhs}, {rhs}")
            } else {
                format!("testflags.{width} {lhs}, {rhs} ; flags={flags}")
            }
        }
        IrInst::EvalCond { dst, cond } => format!("{dst} = evalcond.{cond}"),
        IrInst::Select {
            dst,
            cond,
            if_true,
            if_false,
            width,
        } => {
            format!("{dst} = select.{width} {cond}, {if_true}, {if_false}")
        }
        IrInst::CallHelper { helper, args, ret } => {
            let arg_list = args
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            match ret {
                Some((dst, width)) => format!("{dst} = call.{width} {helper}({arg_list})"),
                None => format!("call {helper}({arg_list})"),
            }
        }
    }
}

fn term_to_text(term: &IrTerminator) -> String {
    match term {
        IrTerminator::Jump { target } => format!("jmp 0x{target:x}"),
        IrTerminator::CondJump {
            cond,
            target,
            fallthrough,
        } => {
            format!("jcc {cond}, 0x{target:x}, 0x{fallthrough:x}")
        }
        IrTerminator::IndirectJump { target } => format!("jmp [{target}]"),
        IrTerminator::ExitToInterpreter { next_rip } => format!("exit_to_interp 0x{next_rip:x}"),
    }
}

pub struct IrBuilder {
    entry_rip: u64,
    insts: Vec<IrInst>,
    value_types: Vec<Width>,
}

impl IrBuilder {
    #[must_use]
    pub fn new(entry_rip: u64) -> Self {
        Self {
            entry_rip,
            insts: Vec::new(),
            value_types: Vec::new(),
        }
    }

    fn fresh(&mut self, width: Width) -> ValueId {
        let id = ValueId(self.value_types.len() as u32);
        self.value_types.push(width);
        id
    }

    #[must_use]
    pub fn const_int(&mut self, width: Width, value: u64) -> ValueId {
        let dst = self.fresh(width);
        self.insts.push(IrInst::Const {
            dst,
            value: width.truncate(value),
            width,
        });
        dst
    }

    #[must_use]
    pub fn read_reg(&mut self, reg: GuestReg) -> ValueId {
        let width = match reg {
            GuestReg::Rip => Width::W64,
            GuestReg::Gpr { width, .. } => width,
            GuestReg::Flag(_) => Width::W8,
        };
        let dst = self.fresh(width);
        self.insts.push(IrInst::ReadReg { dst, reg });
        dst
    }

    pub fn write_reg(&mut self, reg: GuestReg, src: ValueId) {
        self.insts.push(IrInst::WriteReg { reg, src });
    }

    #[must_use]
    pub fn trunc(&mut self, width: Width, src: ValueId) -> ValueId {
        let dst = self.fresh(width);
        self.insts.push(IrInst::Trunc { dst, src, width });
        dst
    }

    #[must_use]
    pub fn load(&mut self, width: Width, addr: ValueId) -> ValueId {
        let dst = self.fresh(width);
        self.insts.push(IrInst::Load { dst, addr, width });
        dst
    }

    pub fn store(&mut self, width: Width, addr: ValueId, src: ValueId) {
        self.insts.push(IrInst::Store { addr, src, width });
    }

    #[must_use]
    pub fn binop(
        &mut self,
        op: BinOp,
        width: Width,
        lhs: ValueId,
        rhs: ValueId,
        flags: FlagSet,
    ) -> ValueId {
        let dst = self.fresh(width);
        self.insts.push(IrInst::BinOp {
            dst,
            op,
            lhs,
            rhs,
            width,
            flags,
        });
        dst
    }

    pub fn cmp_flags(&mut self, width: Width, lhs: ValueId, rhs: ValueId, flags: FlagSet) {
        self.insts.push(IrInst::CmpFlags {
            lhs,
            rhs,
            width,
            flags,
        });
    }

    pub fn test_flags(&mut self, width: Width, lhs: ValueId, rhs: ValueId, flags: FlagSet) {
        self.insts.push(IrInst::TestFlags {
            lhs,
            rhs,
            width,
            flags,
        });
    }

    #[must_use]
    pub fn eval_cond(&mut self, cond: Cond) -> ValueId {
        let dst = self.fresh(Width::W8);
        self.insts.push(IrInst::EvalCond { dst, cond });
        dst
    }

    #[must_use]
    pub fn select(
        &mut self,
        width: Width,
        cond: ValueId,
        if_true: ValueId,
        if_false: ValueId,
    ) -> ValueId {
        let dst = self.fresh(width);
        self.insts.push(IrInst::Select {
            dst,
            cond,
            if_true,
            if_false,
            width,
        });
        dst
    }

    pub fn call_helper(
        &mut self,
        helper: &'static str,
        args: Vec<ValueId>,
        ret: Option<Width>,
    ) -> Option<ValueId> {
        let ret_pair = ret.map(|w| (self.fresh(w), w));
        self.insts.push(IrInst::CallHelper {
            helper,
            args,
            ret: ret_pair,
        });
        ret_pair.map(|(v, _)| v)
    }

    #[must_use]
    pub fn finish(self, terminator: IrTerminator) -> IrBlock {
        IrBlock {
            entry_rip: self.entry_rip,
            insts: self.insts,
            terminator,
            value_types: self.value_types,
        }
    }
}

#[cfg(debug_assertions)]
pub mod interp;
