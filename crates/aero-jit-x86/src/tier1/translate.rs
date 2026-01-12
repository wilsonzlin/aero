use super::block::{BasicBlock, BlockEndKind};
use super::ir::{BinOp, GuestReg, IrBlock, IrBuilder, IrTerminator, ValueId};
use aero_types::{FlagSet, Gpr, Width};
use aero_x86::tier1::{Address, AluOp, DecodedInst, InstKind, Operand, Reg, ShiftOp};

fn gpr_reg(reg: Reg) -> GuestReg {
    GuestReg::Gpr {
        reg: reg.gpr,
        width: reg.width,
        high8: reg.high8,
    }
}

fn emit_address(b: &mut IrBuilder, inst: &DecodedInst, addr: &Address) -> ValueId {
    let next_rip = inst.next_rip();
    let mut acc = if addr.rip_relative {
        b.const_int(Width::W64, next_rip)
    } else if let Some(base) = addr.base {
        b.read_reg(GuestReg::Gpr {
            reg: base,
            width: Width::W64,
            high8: false,
        })
    } else {
        b.const_int(Width::W64, 0)
    };

    if let Some(index) = addr.index {
        let idx = b.read_reg(GuestReg::Gpr {
            reg: index,
            width: Width::W64,
            high8: false,
        });
        let scaled = match addr.scale {
            1 => idx,
            2 => {
                let sh = b.const_int(Width::W64, 1);
                b.binop(BinOp::Shl, Width::W64, idx, sh, FlagSet::EMPTY)
            }
            4 => {
                let sh = b.const_int(Width::W64, 2);
                b.binop(BinOp::Shl, Width::W64, idx, sh, FlagSet::EMPTY)
            }
            8 => {
                let sh = b.const_int(Width::W64, 3);
                b.binop(BinOp::Shl, Width::W64, idx, sh, FlagSet::EMPTY)
            }
            _ => panic!("invalid scale {}", addr.scale),
        };
        acc = b.binop(BinOp::Add, Width::W64, acc, scaled, FlagSet::EMPTY);
    }

    if addr.disp != 0 {
        let disp = b.const_int(Width::W64, addr.disp as i64 as u64);
        acc = b.binop(BinOp::Add, Width::W64, acc, disp, FlagSet::EMPTY);
    }

    acc
}

fn emit_read_operand(b: &mut IrBuilder, inst: &DecodedInst, op: &Operand, width: Width) -> ValueId {
    match op {
        Operand::Imm(v) => b.const_int(width, *v),
        Operand::Reg(r) => {
            debug_assert_eq!(r.width, width);
            b.read_reg(gpr_reg(*r))
        }
        Operand::Mem(addr) => {
            let a = emit_address(b, inst, addr);
            b.load(width, a)
        }
    }
}

fn emit_write_operand(
    b: &mut IrBuilder,
    inst: &DecodedInst,
    op: &Operand,
    width: Width,
    value: ValueId,
) {
    match op {
        Operand::Imm(_) => panic!("cannot write to immediate"),
        Operand::Reg(r) => {
            debug_assert_eq!(r.width, width);
            b.write_reg(gpr_reg(*r), value)
        }
        Operand::Mem(addr) => {
            let a = emit_address(b, inst, addr);
            b.store(width, a, value);
        }
    }
}

fn to_binop(op: AluOp) -> BinOp {
    match op {
        AluOp::Add => BinOp::Add,
        AluOp::Sub => BinOp::Sub,
        AluOp::And => BinOp::And,
        AluOp::Or => BinOp::Or,
        AluOp::Xor => BinOp::Xor,
        AluOp::Shl => BinOp::Shl,
        AluOp::Shr => BinOp::Shr,
        AluOp::Sar => BinOp::Sar,
    }
}

fn to_shift_binop(op: ShiftOp) -> BinOp {
    match op {
        ShiftOp::Shl => BinOp::Shl,
        ShiftOp::Shr => BinOp::Shr,
        ShiftOp::Sar => BinOp::Sar,
    }
}

#[must_use]
pub fn translate_block(block: &BasicBlock) -> IrBlock {
    let mut b = IrBuilder::new(block.entry_rip);

    // Default terminator: if we run off the end, keep executing in interpreter.
    let mut terminator = match block.end_kind {
        BlockEndKind::Limit { next_rip } | BlockEndKind::ExitToInterpreter { next_rip } => {
            IrTerminator::ExitToInterpreter { next_rip }
        }
        _ => IrTerminator::ExitToInterpreter {
            next_rip: block.entry_rip,
        },
    };

    for inst in &block.insts {
        match &inst.kind {
            InstKind::Mov { dst, src, width } => {
                let v = emit_read_operand(&mut b, inst, src, *width);
                emit_write_operand(&mut b, inst, dst, *width, v);
            }
            InstKind::Lea { dst, addr, width } => {
                let a = emit_address(&mut b, inst, addr);
                let dst_reg = GuestReg::Gpr {
                    reg: dst.gpr,
                    width: *width,
                    high8: false,
                };
                let val = if *width == Width::W64 {
                    a
                } else {
                    b.trunc(*width, a)
                };
                b.write_reg(dst_reg, val);
            }
            InstKind::Alu {
                op,
                dst,
                src,
                width,
            } => {
                let lhs = emit_read_operand(&mut b, inst, dst, *width);
                let rhs = emit_read_operand(&mut b, inst, src, *width);
                let res = b.binop(to_binop(*op), *width, lhs, rhs, FlagSet::ALU);
                emit_write_operand(&mut b, inst, dst, *width, res);
            }
            InstKind::Shift {
                op,
                dst,
                count,
                width,
            } => {
                let lhs = emit_read_operand(&mut b, inst, dst, *width);
                // Tier1 IR requires LHS/RHS have the same width.
                let rhs = b.const_int(*width, *count as u64);
                // Tier1 translation currently leaves shift flags unchanged, so we do not request
                // any flag updates for the IR binop here.
                let res = b.binop(to_shift_binop(*op), *width, lhs, rhs, FlagSet::EMPTY);
                emit_write_operand(&mut b, inst, dst, *width, res);
            }
            InstKind::Cmp { lhs, rhs, width } => {
                let l = emit_read_operand(&mut b, inst, lhs, *width);
                let r = emit_read_operand(&mut b, inst, rhs, *width);
                b.cmp_flags(*width, l, r, FlagSet::ALU);
            }
            InstKind::Test { lhs, rhs, width } => {
                let l = emit_read_operand(&mut b, inst, lhs, *width);
                let r = emit_read_operand(&mut b, inst, rhs, *width);
                b.test_flags(*width, l, r, FlagSet::ALU);
            }
            InstKind::Inc { dst, width } => {
                let one = b.const_int(*width, 1);
                let lhs = emit_read_operand(&mut b, inst, dst, *width);
                let res = b.binop(
                    BinOp::Add,
                    *width,
                    lhs,
                    one,
                    FlagSet::ALU.without(FlagSet::CF),
                );
                emit_write_operand(&mut b, inst, dst, *width, res);
            }
            InstKind::Dec { dst, width } => {
                let one = b.const_int(*width, 1);
                let lhs = emit_read_operand(&mut b, inst, dst, *width);
                let res = b.binop(
                    BinOp::Sub,
                    *width,
                    lhs,
                    one,
                    FlagSet::ALU.without(FlagSet::CF),
                );
                emit_write_operand(&mut b, inst, dst, *width, res);
            }
            InstKind::Push { src } => {
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let eight = b.const_int(Width::W64, 8);
                let new_rsp = b.binop(BinOp::Sub, Width::W64, rsp, eight, FlagSet::EMPTY);
                b.write_reg(
                    GuestReg::Gpr {
                        reg: Gpr::Rsp,
                        width: Width::W64,
                        high8: false,
                    },
                    new_rsp,
                );
                let v = emit_read_operand(&mut b, inst, src, Width::W64);
                b.store(Width::W64, new_rsp, v);
            }
            InstKind::Pop { dst } => {
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let v = b.load(Width::W64, rsp);
                let eight = b.const_int(Width::W64, 8);
                let new_rsp = b.binop(BinOp::Add, Width::W64, rsp, eight, FlagSet::EMPTY);
                b.write_reg(
                    GuestReg::Gpr {
                        reg: Gpr::Rsp,
                        width: Width::W64,
                        high8: false,
                    },
                    new_rsp,
                );
                emit_write_operand(&mut b, inst, dst, Width::W64, v);
            }
            InstKind::Setcc { cond, dst } => {
                let c = b.eval_cond(*cond);
                // SETcc writes 0/1 into an 8-bit destination.
                emit_write_operand(&mut b, inst, dst, Width::W8, c);
            }
            InstKind::Cmovcc {
                cond,
                dst,
                src,
                width,
            } => {
                let cond_v = b.eval_cond(*cond);
                let dst_reg = GuestReg::Gpr {
                    reg: dst.gpr,
                    width: *width,
                    high8: false,
                };
                let old = b.read_reg(dst_reg);
                let new = emit_read_operand(&mut b, inst, src, *width);
                let sel = b.select(*width, cond_v, new, old);
                b.write_reg(dst_reg, sel);
            }
            InstKind::JmpRel { target } => {
                terminator = IrTerminator::Jump { target: *target };
                break;
            }
            InstKind::JccRel { cond, target } => {
                let c = b.eval_cond(*cond);
                terminator = IrTerminator::CondJump {
                    cond: c,
                    target: *target,
                    fallthrough: inst.next_rip(),
                };
                break;
            }
            InstKind::CallRel { target } => {
                let return_rip = inst.next_rip();
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let eight = b.const_int(Width::W64, 8);
                let new_rsp = b.binop(BinOp::Sub, Width::W64, rsp, eight, FlagSet::EMPTY);
                b.write_reg(
                    GuestReg::Gpr {
                        reg: Gpr::Rsp,
                        width: Width::W64,
                        high8: false,
                    },
                    new_rsp,
                );
                let ret = b.const_int(Width::W64, return_rip);
                b.store(Width::W64, new_rsp, ret);
                terminator = IrTerminator::Jump { target: *target };
                break;
            }
            InstKind::Ret => {
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let target = b.load(Width::W64, rsp);
                let eight = b.const_int(Width::W64, 8);
                let new_rsp = b.binop(BinOp::Add, Width::W64, rsp, eight, FlagSet::EMPTY);
                b.write_reg(
                    GuestReg::Gpr {
                        reg: Gpr::Rsp,
                        width: Width::W64,
                        high8: false,
                    },
                    new_rsp,
                );
                terminator = IrTerminator::IndirectJump { target };
                break;
            }
            InstKind::Invalid => {
                terminator = IrTerminator::ExitToInterpreter {
                    next_rip: inst.rip,
                };
                break;
            }
        }
    }

    let block = b.finish(terminator);
    if let Err(e) = block.validate() {
        panic!("invalid IR generated: {e}\n{}", block.to_text());
    }
    block
}
