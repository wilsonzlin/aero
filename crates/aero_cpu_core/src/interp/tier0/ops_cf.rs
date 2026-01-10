use super::ops_data::{calc_ea, op_bits, read_op_sized};
use super::ExecOutcome;
use crate::exception::{AssistReason, Exception};
use crate::mem::CpuBus;
use crate::state::{mask_bits, CpuState, FLAG_ZF};
use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Jmp
            | Mnemonic::Call
            | Mnemonic::Ret
            | Mnemonic::Retf
            | Mnemonic::Jo
            | Mnemonic::Jno
            | Mnemonic::Jb
            | Mnemonic::Jae
            | Mnemonic::Je
            | Mnemonic::Jne
            | Mnemonic::Jbe
            | Mnemonic::Ja
            | Mnemonic::Js
            | Mnemonic::Jns
            | Mnemonic::Jp
            | Mnemonic::Jnp
            | Mnemonic::Jl
            | Mnemonic::Jge
            | Mnemonic::Jle
            | Mnemonic::Jg
            | Mnemonic::Loop
            | Mnemonic::Loope
            | Mnemonic::Loopne
            | Mnemonic::Jcxz
            | Mnemonic::Jecxz
            | Mnemonic::Jrcxz
            | Mnemonic::Nop
            | Mnemonic::Pause
            | Mnemonic::Push
            | Mnemonic::Pop
            | Mnemonic::Pusha
            | Mnemonic::Popa
            | Mnemonic::Pushf
            | Mnemonic::Popf
    )
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    match instr.mnemonic() {
        Mnemonic::Nop | Mnemonic::Pause => Ok(ExecOutcome::Continue),
        Mnemonic::Jmp => {
            if is_far_branch(instr) {
                return Ok(ExecOutcome::Assist(AssistReason::Privileged));
            }
            let target = branch_target(state, bus, instr, next_ip)?;
            state.set_rip(target);
            Ok(ExecOutcome::Branch)
        }
        Mnemonic::Call => {
            if is_far_branch(instr) {
                return Ok(ExecOutcome::Assist(AssistReason::Privileged));
            }
            let ret_size = state.bitness() / 8;
            push(state, bus, next_ip, ret_size as u32)?;
            let target = branch_target(state, bus, instr, next_ip)?;
            state.set_rip(target);
            Ok(ExecOutcome::Branch)
        }
        Mnemonic::Ret => {
            let pop_imm = if instr.op_count() == 1 && instr.op_kind(0) == OpKind::Immediate16 {
                instr.immediate16() as u32
            } else {
                0
            };
            let ret_size = state.bitness() / 8;
            let target = pop(state, bus, ret_size as u32)? & state.mode.ip_mask();
            let sp = state.stack_ptr().wrapping_add(pop_imm as u64);
            state.set_stack_ptr(sp);
            state.set_rip(target);
            Ok(ExecOutcome::Branch)
        }
        Mnemonic::Retf => Ok(ExecOutcome::Assist(AssistReason::Privileged)),
        Mnemonic::Push => {
            let bits = op_bits(state, instr, 0)?;
            let v = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
            push(state, bus, v, (bits / 8) as u32)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pop => {
            if state.mode != crate::state::CpuMode::Bit16
                && instr.op_kind(0) == OpKind::Register
                && is_seg_reg(instr.op0_register())
            {
                return Ok(ExecOutcome::Assist(AssistReason::Privileged));
            }
            let bits = op_bits(state, instr, 0)?;
            let v = pop(state, bus, (bits / 8) as u32)?;
            super::ops_data::write_op_sized(state, bus, instr, 0, v, bits, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pusha => {
            let bits = state.bitness();
            if bits != 16 && bits != 32 {
                return Err(Exception::InvalidOpcode);
            }
            let sz = bits / 8;
            let sp_before = state.stack_ptr();
            let regs = if bits == 16 {
                [
                    Register::AX,
                    Register::CX,
                    Register::DX,
                    Register::BX,
                    Register::SP,
                    Register::BP,
                    Register::SI,
                    Register::DI,
                ]
                .to_vec()
            } else {
                [
                    Register::EAX,
                    Register::ECX,
                    Register::EDX,
                    Register::EBX,
                    Register::ESP,
                    Register::EBP,
                    Register::ESI,
                    Register::EDI,
                ]
                .to_vec()
            };
            for r in regs {
                let v = if (bits == 16 && r == Register::SP) || (bits == 32 && r == Register::ESP) {
                    sp_before
                } else {
                    state.read_reg(r)
                };
                push(state, bus, v, sz)?;
            }
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Popa => {
            let bits = state.bitness();
            if bits != 16 && bits != 32 {
                return Err(Exception::InvalidOpcode);
            }
            let sz = bits / 8;
            let regs = if bits == 16 {
                [
                    Register::DI,
                    Register::SI,
                    Register::BP,
                    Register::SP,
                    Register::BX,
                    Register::DX,
                    Register::CX,
                    Register::AX,
                ]
                .to_vec()
            } else {
                [
                    Register::EDI,
                    Register::ESI,
                    Register::EBP,
                    Register::ESP,
                    Register::EBX,
                    Register::EDX,
                    Register::ECX,
                    Register::EAX,
                ]
                .to_vec()
            };
            for r in regs {
                let v = pop(state, bus, sz)?;
                // POPA ignores the value popped into SP/ESP.
                if (bits == 16 && r == Register::SP) || (bits == 32 && r == Register::ESP) {
                    continue;
                }
                state.write_reg(r, v);
            }
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pushf => {
            let bits = state.bitness();
            let v = state.rflags() & mask_bits(bits);
            push(state, bus, v, (bits / 8) as u32)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Popf => {
            let bits = state.bitness();
            let v = pop(state, bus, (bits / 8) as u32)?;
            // Only a subset of flags are relevant for Tier-0 integer execution.
            let mask = crate::state::FLAG_CF
                | crate::state::FLAG_PF
                | crate::state::FLAG_AF
                | crate::state::FLAG_ZF
                | crate::state::FLAG_SF
                | crate::state::FLAG_DF
                | crate::state::FLAG_OF;
            let new = (state.rflags() & !mask) | (v & mask);
            state.set_rflags(new);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Loop | Mnemonic::Loope | Mnemonic::Loopne => {
            let addr_bits = state.bitness();
            let reg = match addr_bits {
                16 => Register::CX,
                32 => Register::ECX,
                64 => Register::RCX,
                _ => Register::ECX,
            };
            let mut count = state.read_reg(reg) & mask_bits(addr_bits);
            count = count.wrapping_sub(1) & mask_bits(addr_bits);
            state.write_reg(reg, count);
            let zf = state.get_flag(FLAG_ZF);
            let cond = match instr.mnemonic() {
                Mnemonic::Loop => count != 0,
                Mnemonic::Loope => count != 0 && zf,
                Mnemonic::Loopne => count != 0 && !zf,
                _ => false,
            };
            if cond {
                let target = instr.near_branch_target();
                state.set_rip(target);
            } else {
                state.set_rip(next_ip);
            }
            Ok(ExecOutcome::Branch)
        }
        Mnemonic::Jcxz | Mnemonic::Jecxz | Mnemonic::Jrcxz => {
            let (reg, bits) = match instr.mnemonic() {
                Mnemonic::Jcxz => (Register::CX, 16),
                Mnemonic::Jecxz => (Register::ECX, 32),
                Mnemonic::Jrcxz => (Register::RCX, 64),
                _ => (Register::ECX, 32),
            };
            let v = state.read_reg(reg) & mask_bits(bits);
            if v == 0 {
                state.set_rip(instr.near_branch_target());
            } else {
                state.set_rip(next_ip);
            }
            Ok(ExecOutcome::Branch)
        }
        m if is_jcc(m) => {
            if super::ops_data::eval_cond(state, m) {
                state.set_rip(instr.near_branch_target());
            } else {
                state.set_rip(next_ip);
            }
            Ok(ExecOutcome::Branch)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn is_far_branch(instr: &Instruction) -> bool {
    matches!(instr.op_kind(0), OpKind::FarBranch16 | OpKind::FarBranch32)
}

fn is_seg_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::ES | Register::CS | Register::SS | Register::DS | Register::FS | Register::GS
    )
}

fn is_jcc(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Jo
            | Mnemonic::Jno
            | Mnemonic::Jb
            | Mnemonic::Jae
            | Mnemonic::Je
            | Mnemonic::Jne
            | Mnemonic::Jbe
            | Mnemonic::Ja
            | Mnemonic::Js
            | Mnemonic::Jns
            | Mnemonic::Jp
            | Mnemonic::Jnp
            | Mnemonic::Jl
            | Mnemonic::Jge
            | Mnemonic::Jle
            | Mnemonic::Jg
    )
}

fn branch_target<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<u64, Exception> {
    match instr.op_kind(0) {
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            Ok(instr.near_branch_target())
        }
        OpKind::Register => Ok(state.read_reg(instr.op0_register()) & state.mode.ip_mask()),
        OpKind::Memory => {
            let bits = op_bits(state, instr, 0)?;
            let addr = calc_ea(state, instr, next_ip, true)?;
            let v = super::ops_data::read_mem(bus, addr, bits)?;
            Ok(v & state.mode.ip_mask())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn push<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    val: u64,
    size: u32,
) -> Result<(), Exception> {
    let sp_bits = state.stack_ptr_bits();
    let mut sp = state.stack_ptr();
    sp = sp.wrapping_sub(size as u64) & mask_bits(sp_bits);
    state.set_stack_ptr(sp);
    let addr = state.seg_base_reg(Register::SS).wrapping_add(sp);
    match size {
        2 => bus.write_u16(addr, val as u16),
        4 => bus.write_u32(addr, val as u32),
        8 => bus.write_u64(addr, val),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn pop<B: CpuBus>(state: &mut CpuState, bus: &mut B, size: u32) -> Result<u64, Exception> {
    let sp_bits = state.stack_ptr_bits();
    let sp = state.stack_ptr();
    let addr = state.seg_base_reg(Register::SS).wrapping_add(sp);
    let v = match size {
        2 => bus.read_u16(addr)? as u64,
        4 => bus.read_u32(addr)? as u64,
        8 => bus.read_u64(addr)?,
        _ => return Err(Exception::InvalidOpcode),
    };
    let new_sp = sp.wrapping_add(size as u64) & mask_bits(sp_bits);
    state.set_stack_ptr(new_sp);
    Ok(v)
}
