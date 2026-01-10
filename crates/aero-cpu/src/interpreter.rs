use aero_jit::block::CodeSource;
use aero_jit::cpu::{CpuState, Flag, FlagOp, Reg};
use aero_jit::x86::{Cond, DecodeError, Decoder, InstKind, MemOperand, Operand64};

use crate::memory::Memory;

#[derive(Debug)]
pub enum ExecError {
    FetchFailed(u64),
    Decode(DecodeError),
    Unsupported(&'static str),
}

/// Tier-0 interpreter for the current x86 subset.
#[derive(Clone, Debug, Default)]
pub struct Interpreter {
    decoder: Decoder,
}

impl Interpreter {
    pub fn step(&self, cpu: &mut CpuState, mem: &mut Memory) -> Result<bool, ExecError> {
        if cpu.is_halted() {
            return Ok(false);
        }

        let rip = cpu.rip;
        let bytes = mem.fetch_code(rip, 15).ok_or(ExecError::FetchFailed(rip))?;
        let inst = self.decoder.decode(bytes, rip).map_err(ExecError::Decode)?;

        let mut wrote_code_page = false;
        let rip_next = rip.wrapping_add(inst.len as u64);

        match inst.kind {
            InstKind::Nop => {
                cpu.rip = rip_next;
            }
            InstKind::Hlt => {
                cpu.set_halted();
                cpu.rip = rip_next;
            }
            InstKind::Mov64 { dst, src } => {
                let val = self.read_op64(cpu, mem, &src, rip_next)?;
                wrote_code_page |= self.write_op64(cpu, mem, &dst, rip_next, val)?;
                cpu.rip = rip_next;
            }
            InstKind::Add64 { dst, src } => {
                wrote_code_page |= self.exec_addsub(cpu, mem, FlagOp::Add, dst, src, rip_next)?;
                cpu.rip = rip_next;
            }
            InstKind::Sub64 { dst, src } => {
                wrote_code_page |= self.exec_addsub(cpu, mem, FlagOp::Sub, dst, src, rip_next)?;
                cpu.rip = rip_next;
            }
            InstKind::Cmp64 { lhs, rhs } => {
                let lhs_val = self.read_op64(cpu, mem, &lhs, rip_next)?;
                let rhs_val = self.read_op64(cpu, mem, &rhs, rip_next)?;
                let result = lhs_val.wrapping_sub(rhs_val);
                cpu.set_pending_flags(FlagOp::Sub, 64, lhs_val, rhs_val, result);
                cpu.rip = rip_next;
            }
            InstKind::Jmp { target } => {
                cpu.rip = target;
            }
            InstKind::Jcc {
                cond,
                target,
                fallthrough,
            } => {
                let zf = cpu.read_flag(Flag::Zf);
                let take = match cond {
                    Cond::Eq => zf,
                    Cond::Ne => !zf,
                };
                cpu.rip = if take { target } else { fallthrough };
            }
            InstKind::Ret => {
                let rsp = cpu.reg(Reg::Rsp);
                let addr = mem.read_u64(rsp);
                cpu.set_reg(Reg::Rsp, rsp.wrapping_add(8));
                cpu.rip = addr;
            }
        }

        Ok(wrote_code_page)
    }

    fn exec_addsub(
        &self,
        cpu: &mut CpuState,
        mem: &mut Memory,
        op: FlagOp,
        dst: Operand64,
        src: Operand64,
        rip_next: u64,
    ) -> Result<bool, ExecError> {
        let lhs = self.read_op64(cpu, mem, &dst, rip_next)?;
        let rhs = self.read_op64(cpu, mem, &src, rip_next)?;
        let result = match op {
            FlagOp::Add => lhs.wrapping_add(rhs),
            FlagOp::Sub => lhs.wrapping_sub(rhs),
            FlagOp::Logic => unreachable!(),
        };
        let wrote_code_page = self.write_op64(cpu, mem, &dst, rip_next, result)?;
        cpu.set_pending_flags(op, 64, lhs, rhs, result);
        Ok(wrote_code_page)
    }

    fn read_op64(
        &self,
        cpu: &CpuState,
        mem: &Memory,
        op: &Operand64,
        rip_next: u64,
    ) -> Result<u64, ExecError> {
        match op {
            Operand64::Reg(r) => Ok(cpu.reg(*r)),
            Operand64::Imm(i) => Ok(*i as u64),
            Operand64::Mem(m) => {
                let addr = eff_addr(cpu, m, rip_next);
                Ok(mem.read_u64(addr))
            }
        }
    }

    fn write_op64(
        &self,
        cpu: &mut CpuState,
        mem: &mut Memory,
        op: &Operand64,
        rip_next: u64,
        val: u64,
    ) -> Result<bool, ExecError> {
        match op {
            Operand64::Reg(r) => {
                cpu.set_reg(*r, val);
                Ok(false)
            }
            Operand64::Mem(m) => {
                let addr = eff_addr(cpu, m, rip_next);
                Ok(mem.write_u64(addr, val))
            }
            Operand64::Imm(_) => Err(ExecError::Unsupported("cannot write to immediate")),
        }
    }
}

fn eff_addr(cpu: &CpuState, mem: &MemOperand, rip_next: u64) -> u64 {
    let mut addr = if mem.rip_relative { rip_next } else { 0 };
    if let Some(base) = mem.base {
        addr = addr.wrapping_add(cpu.reg(base));
    }
    if let Some(index) = mem.index {
        addr = addr.wrapping_add(cpu.reg(index).wrapping_mul(mem.scale as u64));
    }
    addr = addr.wrapping_add(mem.disp as i64 as u64);
    addr
}
