use crate::Reg;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Temp(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Reg(Reg),
    Temp(Temp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Imm(i64),
    Reg(Reg),
    Temp(Temp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Shl,
    ShrU,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    LtS,
    LtU,
    LeS,
    LeU,
    GtS,
    GtU,
    GeS,
    GeU,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemSize {
    U8,
    U16,
    U32,
    U64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IrOp {
    Set {
        dst: Place,
        src: Operand,
    },
    Bin {
        dst: Place,
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
    },
    Cmp {
        dst: Place,
        op: CmpOp,
        lhs: Operand,
        rhs: Operand,
    },
    Select {
        dst: Place,
        cond: Operand,
        if_true: Operand,
        if_false: Operand,
    },
    Load {
        dst: Place,
        addr: Operand,
        size: MemSize,
    },
    Store {
        addr: Operand,
        value: Operand,
        size: MemSize,
    },
    /// Exit the block and return the computed `next_rip`.
    Exit {
        next_rip: Operand,
    },
    /// Exit the block if `cond != 0`.
    ExitIf {
        cond: Operand,
        next_rip: Operand,
    },
    /// Exit to the runtime (interpreter) with a `kind` tag.
    Bailout {
        kind: i32,
        rip: Operand,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrBlock {
    pub ops: Vec<IrOp>,
    pub temp_count: u32,
}

impl IrBlock {
    pub fn new(ops: Vec<IrOp>) -> Self {
        let mut max_temp: Option<u32> = None;
        for op in &ops {
            op.visit_temps(|t| {
                max_temp = Some(max_temp.map_or(t, |cur| cur.max(t)));
            });
        }
        let temp_count = max_temp.map_or(0, |t| t + 1);
        Self { ops, temp_count }
    }
}

impl IrOp {
    fn visit_temps(&self, mut f: impl FnMut(u32)) {
        fn visit_place(p: &Place, f: &mut impl FnMut(u32)) {
            if let Place::Temp(t) = p {
                f(t.0);
            }
        }

        fn visit_operand(o: &Operand, f: &mut impl FnMut(u32)) {
            if let Operand::Temp(t) = o {
                f(t.0);
            }
        }
        match self {
            Self::Set { dst, src } => {
                visit_place(dst, &mut f);
                visit_operand(src, &mut f);
            }
            Self::Bin { dst, lhs, rhs, .. } => {
                visit_place(dst, &mut f);
                visit_operand(lhs, &mut f);
                visit_operand(rhs, &mut f);
            }
            Self::Cmp { dst, lhs, rhs, .. } => {
                visit_place(dst, &mut f);
                visit_operand(lhs, &mut f);
                visit_operand(rhs, &mut f);
            }
            Self::Select {
                dst,
                cond,
                if_true,
                if_false,
            } => {
                visit_place(dst, &mut f);
                visit_operand(cond, &mut f);
                visit_operand(if_true, &mut f);
                visit_operand(if_false, &mut f);
            }
            Self::Load { dst, addr, .. } => {
                visit_place(dst, &mut f);
                visit_operand(addr, &mut f);
            }
            Self::Store { addr, value, .. } => {
                visit_operand(addr, &mut f);
                visit_operand(value, &mut f);
            }
            Self::Exit { next_rip } => {
                visit_operand(next_rip, &mut f);
            }
            Self::ExitIf { cond, next_rip } => {
                visit_operand(cond, &mut f);
                visit_operand(next_rip, &mut f);
            }
            Self::Bailout { rip, .. } => {
                visit_operand(rip, &mut f);
            }
        }
    }
}
