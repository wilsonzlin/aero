//! Debug-only IR interpreter used for validating the x86→IR translation.

use super::{BinOp, GuestReg, IrBlock, IrInst, IrTerminator};
use aero_cpu::{CpuBus, CpuState};
use aero_types::{Cond, Flag, FlagSet, Width};

#[derive(Debug, Clone, Copy)]
struct FlagVals {
    cf: bool,
    pf: bool,
    af: bool,
    zf: bool,
    sf: bool,
    of: bool,
}

fn parity_even(byte: u8) -> bool {
    byte.count_ones() % 2 == 0
}

fn compute_logic_flags(width: Width, result: u64) -> FlagVals {
    let r = width.truncate(result);
    let sign_bit = 1u64 << (width.bits() - 1);
    FlagVals {
        cf: false,
        pf: parity_even(r as u8),
        af: false,
        zf: r == 0,
        sf: (r & sign_bit) != 0,
        of: false,
    }
}

fn compute_add_flags(width: Width, lhs: u64, rhs: u64, result: u64) -> FlagVals {
    let mask = width.mask();
    let lhs = lhs & mask;
    let rhs = rhs & mask;
    let result = result & mask;
    let bits = width.bits();
    let sign_bit = 1u64 << (bits - 1);

    let wide = (lhs as u128) + (rhs as u128);
    let cf = (wide >> bits) != 0;
    let of = ((lhs ^ result) & (rhs ^ result) & sign_bit) != 0;
    let af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    FlagVals {
        cf,
        pf: parity_even(result as u8),
        af,
        zf: result == 0,
        sf: (result & sign_bit) != 0,
        of,
    }
}

fn compute_sub_flags(width: Width, lhs: u64, rhs: u64, result: u64) -> FlagVals {
    let mask = width.mask();
    let lhs = lhs & mask;
    let rhs = rhs & mask;
    let result = result & mask;
    let bits = width.bits();
    let sign_bit = 1u64 << (bits - 1);

    let cf = lhs < rhs;
    let of = ((lhs ^ rhs) & (lhs ^ result) & sign_bit) != 0;
    let af = ((lhs ^ rhs ^ result) & 0x10) != 0;
    FlagVals {
        cf,
        pf: parity_even(result as u8),
        af,
        zf: result == 0,
        sf: (result & sign_bit) != 0,
        of,
    }
}

fn write_flagset(cpu: &mut CpuState, mask: FlagSet, vals: FlagVals) {
    if mask.contains(FlagSet::CF) {
        cpu.write_flag(Flag::Cf, vals.cf);
    }
    if mask.contains(FlagSet::PF) {
        cpu.write_flag(Flag::Pf, vals.pf);
    }
    if mask.contains(FlagSet::AF) {
        cpu.write_flag(Flag::Af, vals.af);
    }
    if mask.contains(FlagSet::ZF) {
        cpu.write_flag(Flag::Zf, vals.zf);
    }
    if mask.contains(FlagSet::SF) {
        cpu.write_flag(Flag::Sf, vals.sf);
    }
    if mask.contains(FlagSet::OF) {
        cpu.write_flag(Flag::Of, vals.of);
    }
}

fn eval_cond(cpu: &CpuState, cond: Cond) -> bool {
    cond.eval(
        cpu.read_flag(Flag::Cf),
        cpu.read_flag(Flag::Pf),
        cpu.read_flag(Flag::Zf),
        cpu.read_flag(Flag::Sf),
        cpu.read_flag(Flag::Of),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecResult {
    Continue,
    ExitToInterpreter { next_rip: u64 },
}

pub fn execute_block<B: CpuBus>(block: &IrBlock, cpu: &mut CpuState, bus: &mut B) -> ExecResult {
    let mut temps = vec![0u64; block.value_types.len()];

    for inst in &block.insts {
        match inst {
            IrInst::Const { dst, value, width } => {
                temps[dst.0 as usize] = width.truncate(*value);
            }
            IrInst::ReadReg { dst, reg } => {
                let v = match *reg {
                    GuestReg::Rip => cpu.rip,
                    GuestReg::Gpr { reg, width, high8 } => cpu.read_gpr_part(reg, width, high8),
                    GuestReg::Flag(flag) => cpu.read_flag(flag) as u64,
                };
                temps[dst.0 as usize] = v;
            }
            IrInst::WriteReg { reg, src } => {
                let v = temps[src.0 as usize];
                match *reg {
                    GuestReg::Rip => cpu.rip = v,
                    GuestReg::Gpr { reg, width, high8 } => cpu.write_gpr_part(reg, width, high8, v),
                    GuestReg::Flag(flag) => cpu.write_flag(flag, (v & 1) != 0),
                }
            }
            IrInst::Trunc { dst, src, width } => {
                let v = temps[src.0 as usize];
                temps[dst.0 as usize] = width.truncate(v);
            }
            IrInst::Load { dst, addr, width } => {
                let a = temps[addr.0 as usize];
                temps[dst.0 as usize] = bus.read(a, *width);
            }
            IrInst::Store { addr, src, width } => {
                let a = temps[addr.0 as usize];
                let v = temps[src.0 as usize];
                bus.write(a, *width, v);
            }
            IrInst::BinOp { dst, op, lhs, rhs, width, flags } => {
                let l = temps[lhs.0 as usize];
                let r = temps[rhs.0 as usize];
                let w = *width;
                let shift_mask = (w.bits() - 1) as u32;
                let (res, flag_vals) = match op {
                    BinOp::Add => {
                        let res = w.truncate(l.wrapping_add(r));
                        (res, Some(compute_add_flags(w, l, r, res)))
                    }
                    BinOp::Sub => {
                        let res = w.truncate(l.wrapping_sub(r));
                        (res, Some(compute_sub_flags(w, l, r, res)))
                    }
                    BinOp::And => {
                        let res = w.truncate(l & r);
                        (res, Some(compute_logic_flags(w, res)))
                    }
                    BinOp::Or => {
                        let res = w.truncate(l | r);
                        (res, Some(compute_logic_flags(w, res)))
                    }
                    BinOp::Xor => {
                        let res = w.truncate(l ^ r);
                        (res, Some(compute_logic_flags(w, res)))
                    }
                    BinOp::Shl => {
                        let amt = (r as u32) & shift_mask;
                        let res = w.truncate(l << amt);
                        (res, None)
                    }
                    BinOp::Shr => {
                        let amt = (r as u32) & shift_mask;
                        let res = w.truncate(l >> amt);
                        (res, None)
                    }
                    BinOp::Sar => {
                        let amt = (r as u32) & shift_mask;
                        let signed = w.sign_extend(w.truncate(l)) as i64;
                        let res = w.truncate((signed >> amt) as u64);
                        (res, None)
                    }
                };
                temps[dst.0 as usize] = res;
                if !flags.is_empty() {
                    if let Some(vals) = flag_vals {
                        write_flagset(cpu, *flags, vals);
                    }
                }
            }
            IrInst::CmpFlags { lhs, rhs, width, flags } => {
                let l = temps[lhs.0 as usize];
                let r = temps[rhs.0 as usize];
                let w = *width;
                let res = w.truncate(l.wrapping_sub(r));
                write_flagset(cpu, *flags, compute_sub_flags(w, l, r, res));
            }
            IrInst::TestFlags { lhs, rhs, width, flags } => {
                let l = temps[lhs.0 as usize];
                let r = temps[rhs.0 as usize];
                let w = *width;
                let res = w.truncate(l & r);
                write_flagset(cpu, *flags, compute_logic_flags(w, res));
            }
            IrInst::EvalCond { dst, cond } => {
                temps[dst.0 as usize] = eval_cond(cpu, *cond) as u64;
            }
            IrInst::Select { dst, cond, if_true, if_false, width } => {
                let c = temps[cond.0 as usize] & 1;
                let t = temps[if_true.0 as usize];
                let e = temps[if_false.0 as usize];
                temps[dst.0 as usize] = width.truncate(if c != 0 { t } else { e });
            }
            IrInst::CallHelper { helper, .. } => {
                panic!("helper call not implemented in debug interpreter: {helper}");
            }
        }
    }

    match block.terminator {
        IrTerminator::Jump { target } => {
            cpu.rip = target;
            ExecResult::Continue
        }
        IrTerminator::CondJump { cond, target, fallthrough } => {
            let c = temps[cond.0 as usize] & 1;
            cpu.rip = if c != 0 { target } else { fallthrough };
            ExecResult::Continue
        }
        IrTerminator::IndirectJump { target } => {
            cpu.rip = temps[target.0 as usize];
            ExecResult::Continue
        }
        IrTerminator::ExitToInterpreter { next_rip } => {
            cpu.rip = next_rip;
            ExecResult::ExitToInterpreter { next_rip }
        }
    }
}
