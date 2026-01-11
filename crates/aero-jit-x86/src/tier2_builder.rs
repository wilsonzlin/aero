use std::collections::{HashMap, VecDeque};

use aero_types::{Cond, Flag, FlagSet, Width};

use crate::block::{discover_block, BasicBlock, BlockEndKind, BlockLimits};
use crate::t2_ir::{BinOp, Block, BlockId, Function, Instr, Operand, Terminator, ValueId};
use crate::tier1_ir::{GuestReg, IrBlock, IrInst, IrTerminator};
use crate::translate::translate_block;
use crate::Tier1Bus;

/// Build a Tier-2 [`Function`] CFG by decoding x86 into Tier-1 IR blocks, then lowering into Tier-2
/// IR.
///
/// The resulting [`Function`] is suitable for Tier-2 trace selection/optimization.
#[must_use]
pub fn build_t2_function<B: Tier1Bus>(bus: &B, entry_rip: u64, limits: BlockLimits) -> Function {
    Tier2CfgBuilder::new(bus, limits).build(entry_rip)
}

struct Tier2CfgBuilder<'a, B: Tier1Bus> {
    bus: &'a B,
    limits: BlockLimits,
    rip_to_block: HashMap<u64, BlockId>,
    blocks: Vec<Option<Block>>,
    queue: VecDeque<u64>,
    next_value: u32,
}

impl<'a, B: Tier1Bus> Tier2CfgBuilder<'a, B> {
    fn new(bus: &'a B, limits: BlockLimits) -> Self {
        Self {
            bus,
            limits,
            rip_to_block: HashMap::new(),
            blocks: Vec::new(),
            queue: VecDeque::new(),
            next_value: 0,
        }
    }

    fn build(mut self, entry_rip: u64) -> Function {
        let entry = self.get_or_create_block(entry_rip);

        while let Some(rip) = self.queue.pop_front() {
            let id = self.rip_to_block[&rip];
            if self.blocks[id.index()].is_some() {
                continue;
            }

            let bb = discover_block(self.bus, rip, self.limits);
            let t2_block = self.lower_block(id, &bb);
            self.blocks[id.index()] = Some(t2_block);
        }

        let blocks = self
            .blocks
            .into_iter()
            .map(|b| b.expect("missing block"))
            .collect();
        Function { blocks, entry }
    }

    fn get_or_create_block(&mut self, rip: u64) -> BlockId {
        if let Some(id) = self.rip_to_block.get(&rip).copied() {
            return id;
        }
        let id = BlockId(self.blocks.len() as u32);
        self.rip_to_block.insert(rip, id);
        self.blocks.push(None);
        self.queue.push_back(rip);
        id
    }

    fn lower_block(&mut self, id: BlockId, bb: &BasicBlock) -> Block {
        let code_len = bb
            .insts
            .iter()
            .fold(0u32, |acc, inst| acc.saturating_add(inst.len as u32));
        let ir = translate_block(bb);

        let base = self.next_value;
        self.next_value = self.next_value.wrapping_add(ir.value_types.len() as u32);

        let (instrs, unsupported) = {
            let mut lower = BlockLowerer {
                entry_rip: bb.entry_rip,
                base,
                next_value: &mut self.next_value,
                instrs: Vec::new(),
                unsupported: false,
            };
            lower.lower_block(&ir);
            (lower.instrs, lower.unsupported)
        };

        // If we hit an unsupported operation, conservatively side-exit at the start of the block
        // so that the interpreter can re-execute it from a clean architectural state.
        if unsupported {
            return Block {
                id,
                start_rip: bb.entry_rip,
                code_len,
                instrs: Vec::new(),
                term: Terminator::SideExit { exit_rip: bb.entry_rip },
            };
        }

        let term = lower_terminator(self, bb, &ir, base);

        Block {
            id,
            start_rip: bb.entry_rip,
            code_len,
            instrs,
            term,
        }
    }
}

fn lower_terminator<B: Tier1Bus>(
    builder: &mut Tier2CfgBuilder<'_, B>,
    bb: &BasicBlock,
    ir: &IrBlock,
    base: u32,
) -> Terminator {
    match ir.terminator {
        IrTerminator::Jump { target } => Terminator::Jump(builder.get_or_create_block(target)),
        IrTerminator::CondJump {
            cond,
            target,
            fallthrough,
        } => Terminator::Branch {
            cond: Operand::Value(ValueId(base + cond.0)),
            then_bb: builder.get_or_create_block(target),
            else_bb: builder.get_or_create_block(fallthrough),
        },
        IrTerminator::IndirectJump { .. } => Terminator::SideExit { exit_rip: bb.entry_rip },
        IrTerminator::ExitToInterpreter { next_rip } => match bb.end_kind {
            BlockEndKind::Limit { next_rip } => Terminator::Jump(builder.get_or_create_block(next_rip)),
            _ => Terminator::SideExit { exit_rip: next_rip },
        },
    }
}

struct BlockLowerer<'a> {
    entry_rip: u64,
    base: u32,
    next_value: &'a mut u32,
    instrs: Vec<Instr>,
    unsupported: bool,
}

impl BlockLowerer<'_> {
    fn map_value(&self, v: crate::tier1_ir::ValueId) -> ValueId {
        ValueId(self.base + v.0)
    }

    fn value(&self, v: crate::tier1_ir::ValueId) -> Operand {
        Operand::Value(self.map_value(v))
    }

    fn fresh_temp(&mut self) -> ValueId {
        let id = ValueId(*self.next_value);
        *self.next_value = self.next_value.wrapping_add(1);
        id
    }

    fn lower_block(&mut self, block: &IrBlock) {
        for inst in &block.insts {
            self.lower_inst(inst);
            if self.unsupported {
                return;
            }
        }
    }

    fn lower_inst(&mut self, inst: &IrInst) {
        match *inst {
            IrInst::Const { dst, value, .. } => {
                self.instrs.push(Instr::Const {
                    dst: self.map_value(dst),
                    value,
                });
            }
            IrInst::ReadReg { dst, reg } => self.lower_read_reg(dst, reg),
            IrInst::WriteReg { reg, src } => self.lower_write_reg(reg, src),
            IrInst::Trunc { dst, src, width } => self.lower_trunc(dst, src, width),
            IrInst::Load { dst, addr, width } => {
                self.instrs.push(Instr::LoadMem {
                    dst: self.map_value(dst),
                    addr: self.value(addr),
                    width,
                });
            }
            IrInst::Store { addr, src, width } => {
                self.instrs.push(Instr::StoreMem {
                    addr: self.value(addr),
                    src: self.value(src),
                    width,
                });
            }
            IrInst::BinOp {
                dst,
                op,
                lhs,
                rhs,
                width,
                flags,
            } => self.lower_binop(dst, op, lhs, rhs, width, flags),
            IrInst::CmpFlags {
                lhs,
                rhs,
                width,
                flags,
            } => self.lower_flag_op(BinOp::Sub, lhs, rhs, width, flags),
            IrInst::TestFlags {
                lhs,
                rhs,
                width,
                flags,
            } => self.lower_flag_op(BinOp::And, lhs, rhs, width, flags),
            IrInst::EvalCond { dst, cond } => self.lower_eval_cond(dst, cond),
            IrInst::Select {
                dst,
                cond,
                if_true,
                if_false,
                width,
            } => self.lower_select(dst, cond, if_true, if_false, width),
            IrInst::CallHelper { .. } => self.unsupported = true,
        }
    }

    fn lower_read_reg(&mut self, dst: crate::tier1_ir::ValueId, reg: GuestReg) {
        let dst = self.map_value(dst);
        match reg {
            GuestReg::Rip => {
                self.instrs.push(Instr::Const {
                    dst,
                    value: self.entry_rip,
                });
            }
            GuestReg::Gpr { reg, width, high8 } => {
                if width == Width::W64 && !high8 {
                    self.instrs.push(Instr::LoadReg { dst, reg });
                    return;
                }

                let full = self.fresh_temp();
                self.instrs.push(Instr::LoadReg { dst: full, reg });

                if width == Width::W8 && high8 {
                    let shifted = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: shifted,
                        op: BinOp::Shr,
                        lhs: Operand::Value(full),
                        rhs: Operand::Const(8),
                        flags: FlagSet::EMPTY,
                    });
                    self.instrs.push(Instr::BinOp {
                        dst,
                        op: BinOp::And,
                        lhs: Operand::Value(shifted),
                        rhs: Operand::Const(0xff),
                        flags: FlagSet::EMPTY,
                    });
                    return;
                }

                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(full),
                    rhs: Operand::Const(width.mask()),
                    flags: FlagSet::EMPTY,
                });
            }
            GuestReg::Flag(flag) => {
                self.instrs.push(Instr::LoadFlag { dst, flag });
            }
        }
    }

    fn lower_write_reg(&mut self, reg: GuestReg, src: crate::tier1_ir::ValueId) {
        match reg {
            GuestReg::Rip => {
                self.unsupported = true;
            }
            GuestReg::Flag(_) => {
                // Tier-2 IR does not currently model direct flag writes; they should be expressed
                // via `BinOp`/`CmpFlags`/`TestFlags` flag updates.
                self.unsupported = true;
            }
            GuestReg::Gpr { reg, width, high8 } => {
                let src = self.value(src);

                if width == Width::W64 && !high8 {
                    self.instrs.push(Instr::StoreReg { reg, src });
                    return;
                }

                if width == Width::W32 {
                    // 32-bit writes zero-extend into the full 64-bit register.
                    let masked = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: masked,
                        op: BinOp::And,
                        lhs: src,
                        rhs: Operand::Const(width.mask()),
                        flags: FlagSet::EMPTY,
                    });
                    self.instrs.push(Instr::StoreReg {
                        reg,
                        src: Operand::Value(masked),
                    });
                    return;
                }

                // 8/16-bit writes preserve the upper bits (or bits 8..15 for AH..BH).
                let shift: u32 = if width == Width::W8 && high8 { 8 } else { 0 };
                let field_mask = if shift == 0 {
                    width.mask()
                } else {
                    0xffu64 << shift
                };
                let preserve_mask = !field_mask;

                let old = self.fresh_temp();
                self.instrs.push(Instr::LoadReg { dst: old, reg });

                let cleared = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: cleared,
                    op: BinOp::And,
                    lhs: Operand::Value(old),
                    rhs: Operand::Const(preserve_mask),
                    flags: FlagSet::EMPTY,
                });

                let masked_src = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: masked_src,
                    op: BinOp::And,
                    lhs: src,
                    rhs: Operand::Const(width.mask()),
                    flags: FlagSet::EMPTY,
                });

                let part = if shift == 0 {
                    Operand::Value(masked_src)
                } else {
                    let shifted = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: shifted,
                        op: BinOp::Shl,
                        lhs: Operand::Value(masked_src),
                        rhs: Operand::Const(shift as u64),
                        flags: FlagSet::EMPTY,
                    });
                    Operand::Value(shifted)
                };

                let new_val = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: new_val,
                    op: BinOp::Or,
                    lhs: Operand::Value(cleared),
                    rhs: part,
                    flags: FlagSet::EMPTY,
                });

                self.instrs.push(Instr::StoreReg {
                    reg,
                    src: Operand::Value(new_val),
                });
            }
        }
    }

    fn lower_trunc(
        &mut self,
        dst: crate::tier1_ir::ValueId,
        src: crate::tier1_ir::ValueId,
        width: Width,
    ) {
        self.instrs.push(Instr::BinOp {
            dst: self.map_value(dst),
            op: BinOp::And,
            lhs: self.value(src),
            rhs: Operand::Const(width.mask()),
            flags: FlagSet::EMPTY,
        });
    }

    fn lower_binop(
        &mut self,
        dst: crate::tier1_ir::ValueId,
        op: crate::tier1_ir::BinOp,
        lhs: crate::tier1_ir::ValueId,
        rhs: crate::tier1_ir::ValueId,
        width: Width,
        flags: FlagSet,
    ) {
        let Some(op) = map_binop(op) else {
            self.unsupported = true;
            return;
        };
        let dst = self.map_value(dst);
        let flags = map_flagset(flags);

        if width == Width::W64 {
            self.instrs.push(Instr::BinOp {
                dst,
                op,
                lhs: self.value(lhs),
                rhs: self.value(rhs),
                flags,
            });
            return;
        }

        match op {
            BinOp::Add | BinOp::Sub | BinOp::And | BinOp::Or | BinOp::Xor => {
                let shift = 64 - width.bits();

                let lhs_s = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: lhs_s,
                    op: BinOp::Shl,
                    lhs: self.value(lhs),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });
                let rhs_s = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: rhs_s,
                    op: BinOp::Shl,
                    lhs: self.value(rhs),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });

                let res_s = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: res_s,
                    op,
                    lhs: Operand::Value(lhs_s),
                    rhs: Operand::Value(rhs_s),
                    flags,
                });

                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Shr,
                    lhs: Operand::Value(res_s),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });
            }
            BinOp::Shl | BinOp::Shr => {
                // Tier-1 shifts are currently used only for address computation; we do not model
                // flag updates for them.
                if !flags.is_empty() {
                    self.unsupported = true;
                    return;
                }

                let mask = width.mask();
                let lhs_masked = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: lhs_masked,
                    op: BinOp::And,
                    lhs: self.value(lhs),
                    rhs: Operand::Const(mask),
                    flags: FlagSet::EMPTY,
                });

                let shifted = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: shifted,
                    op,
                    lhs: Operand::Value(lhs_masked),
                    rhs: self.value(rhs),
                    flags: FlagSet::EMPTY,
                });

                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(shifted),
                    rhs: Operand::Const(mask),
                    flags: FlagSet::EMPTY,
                });
            }
            _ => {
                self.unsupported = true;
            }
        }
    }

    fn lower_flag_op(
        &mut self,
        op: BinOp,
        lhs: crate::tier1_ir::ValueId,
        rhs: crate::tier1_ir::ValueId,
        width: Width,
        flags: FlagSet,
    ) {
        let flags = map_flagset(flags);
        if flags.is_empty() {
            return;
        }

        let dst = self.fresh_temp();

        if width == Width::W64 {
            self.instrs.push(Instr::BinOp {
                dst,
                op,
                lhs: self.value(lhs),
                rhs: self.value(rhs),
                flags,
            });
            return;
        }

        let shift = 64 - width.bits();

        let lhs_s = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: lhs_s,
            op: BinOp::Shl,
            lhs: self.value(lhs),
            rhs: Operand::Const(shift as u64),
            flags: FlagSet::EMPTY,
        });
        let rhs_s = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: rhs_s,
            op: BinOp::Shl,
            lhs: self.value(rhs),
            rhs: Operand::Const(shift as u64),
            flags: FlagSet::EMPTY,
        });

        self.instrs.push(Instr::BinOp {
            dst,
            op,
            lhs: Operand::Value(lhs_s),
            rhs: Operand::Value(rhs_s),
            flags,
        });
    }

    fn lower_eval_cond(&mut self, dst: crate::tier1_ir::ValueId, cond: Cond) {
        let dst = self.map_value(dst);
        match cond {
            Cond::O => self.emit_load_flag(dst, Flag::Of),
            Cond::No => {
                let of = self.load_flag(Flag::Of);
                self.emit_not(dst, Operand::Value(of));
            }
            Cond::B => self.emit_load_flag(dst, Flag::Cf),
            Cond::Ae => {
                let cf = self.load_flag(Flag::Cf);
                self.emit_not(dst, Operand::Value(cf));
            }
            Cond::E => self.emit_load_flag(dst, Flag::Zf),
            Cond::Ne => {
                let zf = self.load_flag(Flag::Zf);
                self.emit_not(dst, Operand::Value(zf));
            }
            Cond::Be => {
                let cf = self.load_flag(Flag::Cf);
                let zf = self.load_flag(Flag::Zf);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Or,
                    lhs: Operand::Value(cf),
                    rhs: Operand::Value(zf),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::A => {
                let cf = self.load_flag(Flag::Cf);
                let zf = self.load_flag(Flag::Zf);
                let not_cf = self.fresh_temp();
                self.emit_not(not_cf, Operand::Value(cf));
                let not_zf = self.fresh_temp();
                self.emit_not(not_zf, Operand::Value(zf));
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(not_cf),
                    rhs: Operand::Value(not_zf),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::S => self.emit_load_flag(dst, Flag::Sf),
            Cond::Ns => {
                let sf = self.load_flag(Flag::Sf);
                self.emit_not(dst, Operand::Value(sf));
            }
            Cond::P => self.emit_load_flag(dst, Flag::Pf),
            Cond::Np => {
                let pf = self.load_flag(Flag::Pf);
                self.emit_not(dst, Operand::Value(pf));
            }
            Cond::L => {
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Xor,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::Ge => {
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Eq,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::Le => {
                let zf = self.load_flag(Flag::Zf);
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                let sfo = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: sfo,
                    op: BinOp::Xor,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Or,
                    lhs: Operand::Value(zf),
                    rhs: Operand::Value(sfo),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::G => {
                let zf = self.load_flag(Flag::Zf);
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                let eq = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: eq,
                    op: BinOp::Eq,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
                let not_zf = self.fresh_temp();
                self.emit_not(not_zf, Operand::Value(zf));
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(not_zf),
                    rhs: Operand::Value(eq),
                    flags: FlagSet::EMPTY,
                });
            }
        }
    }

    fn lower_select(
        &mut self,
        dst: crate::tier1_ir::ValueId,
        cond: crate::tier1_ir::ValueId,
        if_true: crate::tier1_ir::ValueId,
        if_false: crate::tier1_ir::ValueId,
        width: Width,
    ) {
        let dst = self.map_value(dst);
        let cond = self.value(cond);
        let if_true = self.value(if_true);
        let if_false = self.value(if_false);

        let t = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: t,
            op: BinOp::Mul,
            lhs: cond,
            rhs: if_true,
            flags: FlagSet::EMPTY,
        });

        let inv = self.fresh_temp();
        self.emit_not(inv, cond);

        let f = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: f,
            op: BinOp::Mul,
            lhs: Operand::Value(inv),
            rhs: if_false,
            flags: FlagSet::EMPTY,
        });

        if width == Width::W64 {
            self.instrs.push(Instr::BinOp {
                dst,
                op: BinOp::Add,
                lhs: Operand::Value(t),
                rhs: Operand::Value(f),
                flags: FlagSet::EMPTY,
            });
            return;
        }

        let sum = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: sum,
            op: BinOp::Add,
            lhs: Operand::Value(t),
            rhs: Operand::Value(f),
            flags: FlagSet::EMPTY,
        });

        self.instrs.push(Instr::BinOp {
            dst,
            op: BinOp::And,
            lhs: Operand::Value(sum),
            rhs: Operand::Const(width.mask()),
            flags: FlagSet::EMPTY,
        });
    }

    fn load_flag(&mut self, flag: Flag) -> ValueId {
        let dst = self.fresh_temp();
        self.emit_load_flag(dst, flag);
        dst
    }

    fn emit_load_flag(&mut self, dst: ValueId, flag: Flag) {
        self.instrs.push(Instr::LoadFlag { dst, flag });
    }

    fn emit_not(&mut self, dst: ValueId, src: Operand) {
        self.instrs.push(Instr::BinOp {
            dst,
            op: BinOp::Xor,
            lhs: src,
            rhs: Operand::Const(1),
            flags: FlagSet::EMPTY,
        });
    }
}

fn map_flagset(flags: FlagSet) -> FlagSet {
    flags
}

fn map_binop(op: crate::tier1_ir::BinOp) -> Option<BinOp> {
    match op {
        crate::tier1_ir::BinOp::Add => Some(BinOp::Add),
        crate::tier1_ir::BinOp::Sub => Some(BinOp::Sub),
        crate::tier1_ir::BinOp::And => Some(BinOp::And),
        crate::tier1_ir::BinOp::Or => Some(BinOp::Or),
        crate::tier1_ir::BinOp::Xor => Some(BinOp::Xor),
        crate::tier1_ir::BinOp::Shl => Some(BinOp::Shl),
        crate::tier1_ir::BinOp::Shr => Some(BinOp::Shr),
        crate::tier1_ir::BinOp::Sar => None,
    }
}
