use crate::block::BasicBlock;
use crate::cpu::{Flag, FlagOp, Reg};
use crate::x86::{Cond, InstKind, MemOperand, Operand64};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ValueId(pub u32);

impl ValueId {
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    I32,
    I64,
    V128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemSize {
    U8,
    U16,
    U32,
    U64,
}

#[derive(Clone, Debug)]
pub enum IrOp {
    I32Const(i32),
    I64Const(i64),
    I32Eqz {
        value: ValueId,
    },
    I64Add {
        lhs: ValueId,
        rhs: ValueId,
    },
    I64Sub {
        lhs: ValueId,
        rhs: ValueId,
    },
    I64ShlImm {
        value: ValueId,
        shift: u8,
    },
    I64And {
        lhs: ValueId,
        rhs: ValueId,
    },

    LoadReg64 {
        reg: Reg,
    },
    StoreReg64 {
        reg: Reg,
        value: ValueId,
    },

    LoadMem {
        size: MemSize,
        addr: ValueId,
    },
    StoreMem {
        size: MemSize,
        addr: ValueId,
        value: ValueId,
    },

    /// Store a pending-flags record in the CPU state (lazy flags).
    SetPendingFlags {
        op: FlagOp,
        width_bits: u8,
        lhs: ValueId,
        rhs: ValueId,
        result: ValueId,
    },
    /// Read a flag (materializing pending flags if present).
    GetFlag {
        flag: Flag,
    },

    SelectI64 {
        cond: ValueId, // i32
        if_true: ValueId,
        if_false: ValueId,
    },

    SetHalted,

    /// Return the next RIP (i64).
    Return {
        next_rip: ValueId,
    },
}

impl IrOp {
    pub fn result_type(&self) -> Option<ValueType> {
        match self {
            IrOp::I32Const(_) => Some(ValueType::I32),
            IrOp::I64Const(_) => Some(ValueType::I64),
            IrOp::I32Eqz { .. } => Some(ValueType::I32),
            IrOp::I64Add { .. } => Some(ValueType::I64),
            IrOp::I64Sub { .. } => Some(ValueType::I64),
            IrOp::I64ShlImm { .. } => Some(ValueType::I64),
            IrOp::I64And { .. } => Some(ValueType::I64),
            IrOp::LoadReg64 { .. } => Some(ValueType::I64),
            IrOp::LoadMem { size, .. } => match size {
                MemSize::U8 | MemSize::U16 | MemSize::U32 => Some(ValueType::I32),
                MemSize::U64 => Some(ValueType::I64),
            },
            IrOp::GetFlag { .. } => Some(ValueType::I32),
            IrOp::SelectI64 { .. } => Some(ValueType::I64),
            IrOp::StoreReg64 { .. }
            | IrOp::StoreMem { .. }
            | IrOp::SetPendingFlags { .. }
            | IrOp::SetHalted
            | IrOp::Return { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct IrBlock {
    pub start_rip: u64,
    pub ops: Vec<IrOp>,
    pub value_types: Vec<ValueType>,
}

#[derive(Clone, Debug)]
pub enum LowerError {
    Unsupported(&'static str),
}

#[derive(Clone, Debug, Default)]
pub struct IrBuilder {
    ops: Vec<IrOp>,
    values: Vec<ValueType>,
    reg_map: [Option<ValueId>; 16],
    reg_dirty: [bool; 16],
}

impl IrBuilder {
    pub fn lower_basic_block(&mut self, block: &BasicBlock) -> Result<IrBlock, LowerError> {
        self.ops.clear();
        self.values.clear();
        self.reg_map = [None; 16];
        self.reg_dirty = [false; 16];

        for inst in &block.insts {
            let rip = inst.rip;
            let rip_next = rip.wrapping_add(inst.len as u64);
            match &inst.kind {
                InstKind::Mov64 { dst, src } => self.lower_mov(dst, src, rip_next)?,
                InstKind::Add64 { dst, src } => {
                    self.lower_addsub(dst, src, FlagOp::Add, rip_next)?
                }
                InstKind::Sub64 { dst, src } => {
                    self.lower_addsub(dst, src, FlagOp::Sub, rip_next)?
                }
                InstKind::Cmp64 { lhs, rhs } => self.lower_cmp(lhs, rhs, rip_next)?,
                InstKind::Jmp { target } => {
                    self.flush_dirty_regs();
                    let next_rip = self.emit_i64_const(*target as i64);
                    self.ops.push(IrOp::Return { next_rip });
                    break;
                }
                InstKind::Jcc {
                    cond,
                    target,
                    fallthrough,
                } => {
                    self.flush_dirty_regs();
                    let zf = self.emit(IrOp::GetFlag { flag: Flag::Zf }).unwrap();
                    let take_branch = match cond {
                        Cond::Eq => zf,
                        Cond::Ne => self.emit(IrOp::I32Eqz { value: zf }).unwrap(),
                    };
                    let if_true = self.emit_i64_const(*target as i64);
                    let if_false = self.emit_i64_const(*fallthrough as i64);
                    let next_rip = self
                        .emit(IrOp::SelectI64 {
                            cond: take_branch,
                            if_true,
                            if_false,
                        })
                        .unwrap();
                    self.ops.push(IrOp::Return { next_rip });
                    break;
                }
                InstKind::Ret => {
                    // RIP = *(u64*)RSP; RSP += 8
                    let rsp = self.read_reg(Reg::Rsp);
                    let ret_addr = self
                        .emit(IrOp::LoadMem {
                            size: MemSize::U64,
                            addr: rsp,
                        })
                        .unwrap();

                    let eight = self.emit_i64_const(8);
                    let new_rsp = self
                        .emit(IrOp::I64Add {
                            lhs: rsp,
                            rhs: eight,
                        })
                        .unwrap();
                    self.write_reg(Reg::Rsp, new_rsp);

                    self.flush_dirty_regs();
                    self.ops.push(IrOp::Return { next_rip: ret_addr });
                    break;
                }
                InstKind::Hlt => {
                    self.flush_dirty_regs();
                    self.ops.push(IrOp::SetHalted);
                    let next_rip = self.emit_i64_const(rip_next as i64);
                    self.ops.push(IrOp::Return { next_rip });
                    break;
                }
                InstKind::Nop => {}
            }
        }

        if !matches!(self.ops.last(), Some(IrOp::Return { .. })) {
            self.flush_dirty_regs();
            let next_rip = self.emit_i64_const(block.end_rip as i64);
            self.ops.push(IrOp::Return { next_rip });
        }

        Ok(IrBlock {
            start_rip: block.start_rip,
            ops: self.ops.clone(),
            value_types: self.values.clone(),
        })
    }

    fn lower_mov(
        &mut self,
        dst: &Operand64,
        src: &Operand64,
        rip_next: u64,
    ) -> Result<(), LowerError> {
        match (dst, src) {
            (Operand64::Reg(dst), Operand64::Reg(src)) => {
                let v = self.read_reg(*src);
                self.write_reg(*dst, v);
            }
            (Operand64::Reg(dst), Operand64::Imm(imm)) => {
                let v = self.emit_i64_const(*imm);
                self.write_reg(*dst, v);
            }
            (Operand64::Reg(dst), Operand64::Mem(mem)) => {
                let addr = self.lower_mem_addr(mem, rip_next)?;
                let v = self
                    .emit(IrOp::LoadMem {
                        size: MemSize::U64,
                        addr,
                    })
                    .unwrap();
                self.write_reg(*dst, v);
            }
            (Operand64::Mem(mem), Operand64::Reg(src)) => {
                let addr = self.lower_mem_addr(mem, rip_next)?;
                let v = self.read_reg(*src);
                self.ops.push(IrOp::StoreMem {
                    size: MemSize::U64,
                    addr,
                    value: v,
                });
            }
            _ => {
                return Err(LowerError::Unsupported(
                    "unsupported MOV operand combination",
                ))
            }
        }
        Ok(())
    }

    fn lower_addsub(
        &mut self,
        dst: &Operand64,
        src: &Operand64,
        op: FlagOp,
        rip_next: u64,
    ) -> Result<(), LowerError> {
        let dst_old = self.lower_read_operand(dst, rip_next)?;
        let src_val = self.lower_read_operand(src, rip_next)?;

        let result = match op {
            FlagOp::Add => self.emit(IrOp::I64Add {
                lhs: dst_old,
                rhs: src_val,
            }),
            FlagOp::Sub => self.emit(IrOp::I64Sub {
                lhs: dst_old,
                rhs: src_val,
            }),
            FlagOp::Logic => None,
        }
        .ok_or(LowerError::Unsupported("failed to emit add/sub"))?;

        self.lower_write_operand(dst, result, rip_next)?;

        self.ops.push(IrOp::SetPendingFlags {
            op,
            width_bits: 64,
            lhs: dst_old,
            rhs: src_val,
            result,
        });

        Ok(())
    }

    fn lower_cmp(
        &mut self,
        lhs: &Operand64,
        rhs: &Operand64,
        rip_next: u64,
    ) -> Result<(), LowerError> {
        let lhs_val = self.lower_read_operand(lhs, rip_next)?;
        let rhs_val = self.lower_read_operand(rhs, rip_next)?;

        let result = self
            .emit(IrOp::I64Sub {
                lhs: lhs_val,
                rhs: rhs_val,
            })
            .ok_or(LowerError::Unsupported("failed to emit sub"))?;

        self.ops.push(IrOp::SetPendingFlags {
            op: FlagOp::Sub,
            width_bits: 64,
            lhs: lhs_val,
            rhs: rhs_val,
            result,
        });
        Ok(())
    }

    fn lower_read_operand(&mut self, op: &Operand64, rip_next: u64) -> Result<ValueId, LowerError> {
        match op {
            Operand64::Reg(r) => Ok(self.read_reg(*r)),
            Operand64::Imm(imm) => Ok(self.emit_i64_const(*imm)),
            Operand64::Mem(mem) => {
                let addr = self.lower_mem_addr(mem, rip_next)?;
                Ok(self
                    .emit(IrOp::LoadMem {
                        size: MemSize::U64,
                        addr,
                    })
                    .unwrap())
            }
        }
    }

    fn lower_write_operand(
        &mut self,
        op: &Operand64,
        value: ValueId,
        rip_next: u64,
    ) -> Result<(), LowerError> {
        match op {
            Operand64::Reg(r) => {
                self.write_reg(*r, value);
                Ok(())
            }
            Operand64::Mem(mem) => {
                let addr = self.lower_mem_addr(mem, rip_next)?;
                self.ops.push(IrOp::StoreMem {
                    size: MemSize::U64,
                    addr,
                    value,
                });
                Ok(())
            }
            _ => Err(LowerError::Unsupported("cannot write to immediate operand")),
        }
    }

    fn lower_mem_addr(&mut self, mem: &MemOperand, rip_next: u64) -> Result<ValueId, LowerError> {
        let mut addr = self.emit_i64_const(0);

        if mem.rip_relative {
            let ripc = self.emit_i64_const(rip_next as i64);
            addr = self
                .emit(IrOp::I64Add {
                    lhs: addr,
                    rhs: ripc,
                })
                .unwrap();
        } else if let Some(base) = mem.base {
            let b = self.read_reg(base);
            addr = self.emit(IrOp::I64Add { lhs: addr, rhs: b }).unwrap();
        }

        if let Some(index) = mem.index {
            let mut ix = self.read_reg(index);
            if mem.scale != 1 {
                let shift = mem.scale.trailing_zeros() as u8;
                ix = self.emit(IrOp::I64ShlImm { value: ix, shift }).unwrap();
            }
            addr = self.emit(IrOp::I64Add { lhs: addr, rhs: ix }).unwrap();
        }

        if mem.disp != 0 {
            let disp = self.emit_i64_const(mem.disp as i64);
            addr = self
                .emit(IrOp::I64Add {
                    lhs: addr,
                    rhs: disp,
                })
                .unwrap();
        }

        Ok(addr)
    }

    fn read_reg(&mut self, reg: Reg) -> ValueId {
        let idx = reg.as_usize();
        if let Some(v) = self.reg_map[idx] {
            return v;
        }
        let v = self.emit(IrOp::LoadReg64 { reg }).unwrap();
        self.reg_map[idx] = Some(v);
        v
    }

    fn write_reg(&mut self, reg: Reg, value: ValueId) {
        let idx = reg.as_usize();
        self.reg_map[idx] = Some(value);
        self.reg_dirty[idx] = true;
    }

    fn flush_dirty_regs(&mut self) {
        for (idx, dirty) in self.reg_dirty.iter_mut().enumerate() {
            if !*dirty {
                continue;
            }
            let reg = Reg::from_u4(idx as u8).unwrap();
            let val = self.reg_map[idx].expect("dirty reg must have value");
            self.ops.push(IrOp::StoreReg64 { reg, value: val });
            *dirty = false;
        }
    }

    fn emit_i64_const(&mut self, v: i64) -> ValueId {
        self.emit(IrOp::I64Const(v)).unwrap()
    }

    fn emit(&mut self, op: IrOp) -> Option<ValueId> {
        let value_ty = op.result_type();
        self.ops.push(op);
        value_ty.map(|ty| {
            let id = ValueId(self.values.len() as u32);
            self.values.push(ty);
            id
        })
    }
}
