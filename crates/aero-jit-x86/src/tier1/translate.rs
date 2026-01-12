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

fn mask_addr(b: &mut IrBuilder, addr: ValueId, addr_mask: Option<ValueId>) -> ValueId {
    if let Some(mask) = addr_mask {
        b.binop(BinOp::And, Width::W64, addr, mask, FlagSet::EMPTY)
    } else {
        addr
    }
}

fn emit_address(
    b: &mut IrBuilder,
    inst: &DecodedInst,
    addr: &Address,
    addr_mask: Option<ValueId>,
) -> ValueId {
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

    mask_addr(b, acc, addr_mask)
}

fn emit_read_operand(
    b: &mut IrBuilder,
    inst: &DecodedInst,
    op: &Operand,
    width: Width,
    addr_mask: Option<ValueId>,
) -> ValueId {
    match op {
        Operand::Imm(v) => b.const_int(width, *v),
        Operand::Reg(r) => {
            debug_assert_eq!(r.width, width);
            b.read_reg(gpr_reg(*r))
        }
        Operand::Mem(addr) => {
            let a = emit_address(b, inst, addr, addr_mask);
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
    addr_mask: Option<ValueId>,
) {
    match op {
        Operand::Imm(_) => panic!("cannot write to immediate"),
        Operand::Reg(r) => {
            debug_assert_eq!(r.width, width);
            b.write_reg(gpr_reg(*r), value)
        }
        Operand::Mem(addr) => {
            let a = emit_address(b, inst, addr, addr_mask);
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
    let stack_width = match block.bitness {
        16 => Width::W16,
        32 => Width::W32,
        64 => Width::W64,
        other => panic!("unsupported Tier-1 bitness {other}"),
    };
    let ip_mask: u64 = match block.bitness {
        16 => 0xffff,
        32 => 0xffff_ffff,
        64 => u64::MAX,
        other => panic!("unsupported Tier-1 bitness {other}"),
    };
    let addr_mask = if ip_mask != u64::MAX {
        Some(b.const_int(Width::W64, ip_mask))
    } else {
        None
    };

    // Default terminator: if we run off the end, keep executing in interpreter.
    let mut terminator = match block.end_kind {
        BlockEndKind::Limit { next_rip } | BlockEndKind::ExitToInterpreter { next_rip } => {
            IrTerminator::ExitToInterpreter {
                next_rip: next_rip & ip_mask,
            }
        }
        _ => IrTerminator::ExitToInterpreter {
            next_rip: block.entry_rip & ip_mask,
        },
    };

    for inst in &block.insts {
        match &inst.kind {
            InstKind::Mov { dst, src, width } => {
                let v = emit_read_operand(&mut b, inst, src, *width, addr_mask);
                emit_write_operand(&mut b, inst, dst, *width, v, addr_mask);
            }
            InstKind::Lea { dst, addr, width } => {
                let a = emit_address(&mut b, inst, addr, addr_mask);
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
                let lhs = emit_read_operand(&mut b, inst, dst, *width, addr_mask);
                let rhs = emit_read_operand(&mut b, inst, src, *width, addr_mask);
                let res = b.binop(to_binop(*op), *width, lhs, rhs, FlagSet::ALU);
                emit_write_operand(&mut b, inst, dst, *width, res, addr_mask);
            }
            InstKind::Shift {
                op,
                dst,
                count,
                width,
            } => {
                let lhs = emit_read_operand(&mut b, inst, dst, *width, addr_mask);
                // Tier1 IR requires LHS/RHS have the same width.
                let rhs = b.const_int(*width, *count as u64);
                // Tier1 translation currently leaves shift flags unchanged, so we do not request
                // any flag updates for the IR binop here.
                let res = b.binop(to_shift_binop(*op), *width, lhs, rhs, FlagSet::EMPTY);
                emit_write_operand(&mut b, inst, dst, *width, res, addr_mask);
            }
            InstKind::Cmp { lhs, rhs, width } => {
                let l = emit_read_operand(&mut b, inst, lhs, *width, addr_mask);
                let r = emit_read_operand(&mut b, inst, rhs, *width, addr_mask);
                b.cmp_flags(*width, l, r, FlagSet::ALU);
            }
            InstKind::Test { lhs, rhs, width } => {
                let l = emit_read_operand(&mut b, inst, lhs, *width, addr_mask);
                let r = emit_read_operand(&mut b, inst, rhs, *width, addr_mask);
                b.test_flags(*width, l, r, FlagSet::ALU);
            }
            InstKind::Inc { dst, width } => {
                let one = b.const_int(*width, 1);
                let lhs = emit_read_operand(&mut b, inst, dst, *width, addr_mask);
                let res = b.binop(
                    BinOp::Add,
                    *width,
                    lhs,
                    one,
                    FlagSet::ALU.without(FlagSet::CF),
                );
                emit_write_operand(&mut b, inst, dst, *width, res, addr_mask);
            }
            InstKind::Dec { dst, width } => {
                let one = b.const_int(*width, 1);
                let lhs = emit_read_operand(&mut b, inst, dst, *width, addr_mask);
                let res = b.binop(
                    BinOp::Sub,
                    *width,
                    lhs,
                    one,
                    FlagSet::ALU.without(FlagSet::CF),
                );
                emit_write_operand(&mut b, inst, dst, *width, res, addr_mask);
            }
            InstKind::Push { src } => {
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let rsp = mask_addr(&mut b, rsp, addr_mask);
                let slot = b.const_int(Width::W64, stack_width.bytes() as u64);
                let new_rsp = b.binop(BinOp::Sub, Width::W64, rsp, slot, FlagSet::EMPTY);
                let new_rsp = mask_addr(&mut b, new_rsp, addr_mask);
                b.write_reg(
                    GuestReg::Gpr {
                        reg: Gpr::Rsp,
                        width: Width::W64,
                        high8: false,
                    },
                    new_rsp,
                );
                let v = emit_read_operand(&mut b, inst, src, stack_width, addr_mask);
                b.store(stack_width, new_rsp, v);
            }
            InstKind::Pop { dst } => {
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let rsp = mask_addr(&mut b, rsp, addr_mask);
                let v = b.load(stack_width, rsp);
                let slot = b.const_int(Width::W64, stack_width.bytes() as u64);
                let new_rsp = b.binop(BinOp::Add, Width::W64, rsp, slot, FlagSet::EMPTY);
                let new_rsp = mask_addr(&mut b, new_rsp, addr_mask);
                b.write_reg(
                    GuestReg::Gpr {
                        reg: Gpr::Rsp,
                        width: Width::W64,
                        high8: false,
                    },
                    new_rsp,
                );
                emit_write_operand(&mut b, inst, dst, stack_width, v, addr_mask);
            }
            InstKind::Setcc { cond, dst } => {
                let c = b.eval_cond(*cond);
                // SETcc writes 0/1 into an 8-bit destination.
                emit_write_operand(&mut b, inst, dst, Width::W8, c, addr_mask);
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
                let new = emit_read_operand(&mut b, inst, src, *width, addr_mask);
                let sel = b.select(*width, cond_v, new, old);
                b.write_reg(dst_reg, sel);
            }
            InstKind::JmpRel { target } => {
                terminator = IrTerminator::Jump {
                    target: *target & ip_mask,
                };
                break;
            }
            InstKind::JccRel { cond, target } => {
                let c = b.eval_cond(*cond);
                terminator = IrTerminator::CondJump {
                    cond: c,
                    target: *target & ip_mask,
                    fallthrough: inst.next_rip() & ip_mask,
                };
                break;
            }
            InstKind::CallRel { target } => {
                let return_rip = inst.next_rip() & ip_mask;
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let rsp = mask_addr(&mut b, rsp, addr_mask);
                let slot = b.const_int(Width::W64, stack_width.bytes() as u64);
                let new_rsp = b.binop(BinOp::Sub, Width::W64, rsp, slot, FlagSet::EMPTY);
                let new_rsp = mask_addr(&mut b, new_rsp, addr_mask);
                b.write_reg(
                    GuestReg::Gpr {
                        reg: Gpr::Rsp,
                        width: Width::W64,
                        high8: false,
                    },
                    new_rsp,
                );
                let ret = b.const_int(stack_width, return_rip);
                b.store(stack_width, new_rsp, ret);
                terminator = IrTerminator::Jump {
                    target: *target & ip_mask,
                };
                break;
            }
            InstKind::Ret => {
                let rsp = b.read_reg(GuestReg::Gpr {
                    reg: Gpr::Rsp,
                    width: Width::W64,
                    high8: false,
                });
                let rsp = mask_addr(&mut b, rsp, addr_mask);
                let target = b.load(stack_width, rsp);
                let slot = b.const_int(Width::W64, stack_width.bytes() as u64);
                let new_rsp = b.binop(BinOp::Add, Width::W64, rsp, slot, FlagSet::EMPTY);
                let new_rsp = mask_addr(&mut b, new_rsp, addr_mask);
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
                    next_rip: inst.rip & ip_mask,
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
