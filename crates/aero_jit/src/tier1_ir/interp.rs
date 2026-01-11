//! Debug-only IR interpreter used for validating the x86→IR translation.

use super::{BinOp, GuestReg, IrBlock, IrInst, IrTerminator};
use aero_cpu_core::state::{
    CpuState, RFLAGS_AF, RFLAGS_CF, RFLAGS_OF, RFLAGS_PF, RFLAGS_SF, RFLAGS_ZF,
};
use aero_types::{Cond, Flag, FlagSet, Width};

use crate::Tier1Bus;

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
        write_flag(cpu, Flag::Cf, vals.cf);
    }
    if mask.contains(FlagSet::PF) {
        write_flag(cpu, Flag::Pf, vals.pf);
    }
    if mask.contains(FlagSet::AF) {
        write_flag(cpu, Flag::Af, vals.af);
    }
    if mask.contains(FlagSet::ZF) {
        write_flag(cpu, Flag::Zf, vals.zf);
    }
    if mask.contains(FlagSet::SF) {
        write_flag(cpu, Flag::Sf, vals.sf);
    }
    if mask.contains(FlagSet::OF) {
        write_flag(cpu, Flag::Of, vals.of);
    }
}

fn eval_cond(cpu: &CpuState, cond: Cond) -> bool {
    cond.eval(
        read_flag(cpu, Flag::Cf),
        read_flag(cpu, Flag::Pf),
        read_flag(cpu, Flag::Zf),
        read_flag(cpu, Flag::Sf),
        read_flag(cpu, Flag::Of),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecResult {
    Continue,
    ExitToInterpreter { next_rip: u64 },
}

pub fn execute_block<B: Tier1Bus>(block: &IrBlock, cpu: &mut CpuState, bus: &mut B) -> ExecResult {
    let mut temps = vec![0u64; block.value_types.len()];

    for inst in &block.insts {
        match inst {
            IrInst::Const { dst, value, width } => {
                temps[dst.0 as usize] = width.truncate(*value);
            }
            IrInst::ReadReg { dst, reg } => {
                let v = match *reg {
                    GuestReg::Rip => cpu.rip,
                    GuestReg::Gpr { reg, width, high8 } => read_gpr_part(cpu, reg, width, high8),
                    GuestReg::Flag(flag) => read_flag(cpu, flag) as u64,
                };
                temps[dst.0 as usize] = v;
            }
            IrInst::WriteReg { reg, src } => {
                let v = temps[src.0 as usize];
                match *reg {
                    GuestReg::Rip => cpu.rip = v,
                    GuestReg::Gpr { reg, width, high8 } => write_gpr_part(cpu, reg, width, high8, v),
                    GuestReg::Flag(flag) => write_flag(cpu, flag, (v & 1) != 0),
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

#[inline]
fn read_gpr_part(cpu: &CpuState, reg: aero_types::Gpr, width: Width, high8: bool) -> u64 {
    let idx = reg.as_u8() as usize;
    let val = cpu.gpr[idx];
    match width {
        Width::W8 => {
            if high8 {
                debug_assert!(matches!(
                    reg,
                    aero_types::Gpr::Rax | aero_types::Gpr::Rcx | aero_types::Gpr::Rdx | aero_types::Gpr::Rbx
                ));
                (val >> 8) & 0xff
            } else {
                val & 0xff
            }
        }
        Width::W16 => val & 0xffff,
        Width::W32 => val & 0xffff_ffff,
        Width::W64 => val,
    }
}

#[inline]
fn write_gpr_part(cpu: &mut CpuState, reg: aero_types::Gpr, width: Width, high8: bool, value: u64) {
    let idx = reg.as_u8() as usize;
    let prev = cpu.gpr[idx];
    let masked = width.truncate(value);
    cpu.gpr[idx] = match width {
        Width::W8 => {
            if high8 {
                debug_assert!(matches!(
                    reg,
                    aero_types::Gpr::Rax | aero_types::Gpr::Rcx | aero_types::Gpr::Rdx | aero_types::Gpr::Rbx
                ));
                (prev & !0xff00) | ((masked & 0xff) << 8)
            } else {
                (prev & !0xff) | (masked & 0xff)
            }
        }
        Width::W16 => (prev & !0xffff) | (masked & 0xffff),
        // Writes to a 32-bit GPR clear the upper 32 bits, even in 64-bit mode.
        Width::W32 => masked & 0xffff_ffff,
        Width::W64 => masked,
    };
}

#[inline]
fn flag_mask(flag: Flag) -> u64 {
    match flag {
        Flag::Cf => RFLAGS_CF,
        Flag::Pf => RFLAGS_PF,
        Flag::Af => RFLAGS_AF,
        Flag::Zf => RFLAGS_ZF,
        Flag::Sf => RFLAGS_SF,
        Flag::Of => RFLAGS_OF,
    }
}

#[inline]
fn read_flag(cpu: &CpuState, flag: Flag) -> bool {
    cpu.get_flag(flag_mask(flag))
}

#[inline]
fn write_flag(cpu: &mut CpuState, flag: Flag, value: bool) {
    cpu.set_flag(flag_mask(flag), value);
}
