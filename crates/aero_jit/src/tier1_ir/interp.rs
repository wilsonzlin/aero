//! Debug-only IR interpreter used for validating the x86→IR translation.

use super::{BinOp, GuestReg, IrBlock, IrInst, IrTerminator};
use aero_cpu::CpuBus;
use aero_types::{Cond, Flag, FlagSet, Gpr, Width};

use crate::abi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestCpu {
    pub gpr: [u64; abi::GPR_COUNT],
    pub rip: u64,
    pub rflags: u64,
}

impl Default for TestCpu {
    fn default() -> Self {
        Self {
            gpr: [0; abi::GPR_COUNT],
            rip: 0,
            rflags: abi::RFLAGS_RESERVED1,
        }
    }
}

impl TestCpu {
    #[must_use]
    pub fn read_flag(&self, flag: Flag) -> bool {
        ((self.rflags >> flag.rflags_bit()) & 1) != 0
    }

    pub fn write_flag(&mut self, flag: Flag, val: bool) {
        let bit = 1u64 << flag.rflags_bit();
        if val {
            self.rflags |= bit;
        } else {
            self.rflags &= !bit;
        }
    }

    #[must_use]
    pub fn read_gpr(&self, reg: Gpr) -> u64 {
        self.gpr[reg.as_u8() as usize]
    }

    pub fn write_gpr(&mut self, reg: Gpr, value: u64) {
        self.gpr[reg.as_u8() as usize] = value;
    }

    /// Read a sub-register (8/16/32/64) from a full 64-bit GPR.
    ///
    /// If `high8` is set for 8-bit accesses, bits 8..=15 (AH/CH/DH/BH) are read.
    #[must_use]
    pub fn read_gpr_part(&self, reg: Gpr, width: Width, high8: bool) -> u64 {
        let val = self.read_gpr(reg);
        match width {
            Width::W8 => {
                if high8 {
                    debug_assert!(matches!(reg, Gpr::Rax | Gpr::Rcx | Gpr::Rdx | Gpr::Rbx));
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

    /// Write a sub-register (8/16/32/64) into a full 64-bit GPR.
    ///
    /// x86-64 semantics:
    /// - 8/16-bit writes only update the low bits (or AH..BH for `high8`).
    /// - 32-bit writes zero-extend into 64-bit.
    pub fn write_gpr_part(&mut self, reg: Gpr, width: Width, high8: bool, value: u64) {
        let idx = reg.as_u8() as usize;
        let prev = self.gpr[idx];
        let masked = width.truncate(value);
        self.gpr[idx] = match width {
            Width::W8 => {
                if high8 {
                    debug_assert!(matches!(reg, Gpr::Rax | Gpr::Rcx | Gpr::Rdx | Gpr::Rbx));
                    (prev & !0xff00) | ((masked & 0xff) << 8)
                } else {
                    (prev & !0xff) | (masked & 0xff)
                }
            }
            Width::W16 => (prev & !0xffff) | (masked & 0xffff),
            Width::W32 => masked & 0xffff_ffff,
            Width::W64 => masked,
        };
    }

    #[must_use]
    pub fn from_abi_mem(mem: &[u8], base: usize) -> Self {
        assert!(
            base + (abi::CPU_STATE_SIZE as usize) <= mem.len(),
            "CpuState ABI read out of bounds: base={base} size={} mem_len={}",
            abi::CPU_STATE_SIZE,
            mem.len()
        );

        let mut gpr = [0u64; abi::GPR_COUNT];
        for (i, slot) in gpr.iter_mut().enumerate() {
            *slot = read_u64_le(mem, base + (abi::CPU_GPR_OFF[i] as usize));
        }
        let rip = read_u64_le(mem, base + (abi::CPU_RIP_OFF as usize));
        let rflags =
            read_u64_le(mem, base + (abi::CPU_RFLAGS_OFF as usize)) | abi::RFLAGS_RESERVED1;

        Self { gpr, rip, rflags }
    }

    pub fn write_to_abi_mem(&self, mem: &mut [u8], base: usize) {
        assert!(
            base + (abi::CPU_STATE_SIZE as usize) <= mem.len(),
            "CpuState ABI write out of bounds: base={base} size={} mem_len={}",
            abi::CPU_STATE_SIZE,
            mem.len()
        );

        for (i, val) in self.gpr.iter().enumerate() {
            write_u64_le(mem, base + (abi::CPU_GPR_OFF[i] as usize), *val);
        }
        write_u64_le(mem, base + (abi::CPU_RIP_OFF as usize), self.rip);
        write_u64_le(
            mem,
            base + (abi::CPU_RFLAGS_OFF as usize),
            self.rflags | abi::RFLAGS_RESERVED1,
        );
    }
}

fn read_u64_le(mem: &[u8], off: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&mem[off..off + 8]);
    u64::from_le_bytes(buf)
}

fn write_u64_le(mem: &mut [u8], off: usize, value: u64) {
    mem[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

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

fn write_flagset(cpu: &mut TestCpu, mask: FlagSet, vals: FlagVals) {
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

fn eval_cond(cpu: &TestCpu, cond: Cond) -> bool {
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

pub fn execute_block<B: CpuBus>(block: &IrBlock, cpu_mem: &mut [u8], bus: &mut B) -> ExecResult {
    let mut cpu = TestCpu::from_abi_mem(cpu_mem, 0);
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
            IrInst::BinOp {
                dst,
                op,
                lhs,
                rhs,
                width,
                flags,
            } => {
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
                        write_flagset(&mut cpu, *flags, vals);
                    }
                }
            }
            IrInst::CmpFlags {
                lhs,
                rhs,
                width,
                flags,
            } => {
                let l = temps[lhs.0 as usize];
                let r = temps[rhs.0 as usize];
                let w = *width;
                let res = w.truncate(l.wrapping_sub(r));
                write_flagset(&mut cpu, *flags, compute_sub_flags(w, l, r, res));
            }
            IrInst::TestFlags {
                lhs,
                rhs,
                width,
                flags,
            } => {
                let l = temps[lhs.0 as usize];
                let r = temps[rhs.0 as usize];
                let w = *width;
                let res = w.truncate(l & r);
                write_flagset(&mut cpu, *flags, compute_logic_flags(w, res));
            }
            IrInst::EvalCond { dst, cond } => {
                temps[dst.0 as usize] = eval_cond(&cpu, *cond) as u64;
            }
            IrInst::Select {
                dst,
                cond,
                if_true,
                if_false,
                width,
            } => {
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
            cpu.write_to_abi_mem(cpu_mem, 0);
            ExecResult::Continue
        }
        IrTerminator::CondJump {
            cond,
            target,
            fallthrough,
        } => {
            let c = temps[cond.0 as usize] & 1;
            cpu.rip = if c != 0 { target } else { fallthrough };
            cpu.write_to_abi_mem(cpu_mem, 0);
            ExecResult::Continue
        }
        IrTerminator::IndirectJump { target } => {
            cpu.rip = temps[target.0 as usize];
            cpu.write_to_abi_mem(cpu_mem, 0);
            ExecResult::Continue
        }
        IrTerminator::ExitToInterpreter { next_rip } => {
            cpu.rip = next_rip;
            cpu.write_to_abi_mem(cpu_mem, 0);
            ExecResult::ExitToInterpreter { next_rip }
        }
    }
}
