use super::ops_data::{calc_ea, op_bits, read_mem, read_op_sized, write_mem};
use super::ExecOutcome;
use crate::exception::Exception;
use crate::linear_mem::{
    contiguous_masked_start, read_u128_wrapped, read_u64_wrapped, write_u128_wrapped,
    write_u64_wrapped,
};
use crate::mem::CpuBus;
use crate::state::{mask_bits, CpuState, FLAG_ZF};
use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Cmpxchg | Mnemonic::Cmpxchg8b | Mnemonic::Cmpxchg16b
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
        Mnemonic::Cmpxchg => exec_cmpxchg(state, bus, instr, next_ip)?,
        Mnemonic::Cmpxchg8b => exec_cmpxchg8b(state, bus, instr, next_ip)?,
        Mnemonic::Cmpxchg16b => exec_cmpxchg16b(state, bus, instr, next_ip)?,
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(ExecOutcome::Continue)
}

fn exec_cmpxchg<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let bits = op_bits(state, instr, 0)?;
    let acc = cmpxchg_acc_reg(bits)?;
    let expected = state.read_reg(acc) & mask_bits(bits);
    let src = read_op_sized(state, bus, instr, 1, bits, next_ip)?;

    match instr.op_kind(0) {
        OpKind::Register => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            let dst_reg = instr.op0_register();
            let dst = state.read_reg(dst_reg) & mask_bits(bits);

            let (_res, flags) = super::ops_alu::sub_with_flags(state, expected, dst, 0, bits);
            state.set_rflags(flags);

            if dst == expected {
                state.write_reg(dst_reg, src);
            } else {
                state.write_reg(acc, dst);
            }
            Ok(())
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            let locked = instr.has_lock_prefix();
            let (old, swapped) = if locked {
                super::atomic_rmw_sized(state, bus, addr, bits, |old| {
                    if old == expected {
                        (src, (old, true))
                    } else {
                        (old, (old, false))
                    }
                })?
            } else {
                let old = read_mem(state, bus, addr, bits)?;
                if old == expected {
                    write_mem(state, bus, addr, bits, src)?;
                    (old, true)
                } else {
                    (old, false)
                }
            };

            let (_res, flags) = super::ops_alu::sub_with_flags(state, expected, old, 0, bits);
            state.set_rflags(flags);

            if !swapped {
                state.write_reg(acc, old);
            }
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_cmpxchg8b<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    if instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }
    let addr = calc_ea(state, instr, next_ip, true)?;

    let expected =
        ((state.read_reg(Register::EDX) as u64) << 32) | (state.read_reg(Register::EAX) as u64);
    let replacement =
        ((state.read_reg(Register::ECX) as u64) << 32) | (state.read_reg(Register::EBX) as u64);

    let (old, swapped) = if instr.has_lock_prefix() {
        if let Some(start) = contiguous_masked_start(state, addr, 8) {
            bus.atomic_rmw::<u64, _>(start, |old| {
                if old == expected {
                    (replacement, (old, true))
                } else {
                    (old, (old, false))
                }
            })?
        } else {
            let old = read_u64_wrapped(state, bus, addr)?;
            if old == expected {
                write_u64_wrapped(state, bus, addr, replacement)?;
                (old, true)
            } else {
                (old, false)
            }
        }
    } else {
        let old = read_u64_wrapped(state, bus, addr)?;
        if old == expected {
            write_u64_wrapped(state, bus, addr, replacement)?;
            (old, true)
        } else {
            (old, false)
        }
    };

    state.set_flag(FLAG_ZF, swapped);
    if !swapped {
        state.write_reg(Register::EAX, old as u32 as u64);
        state.write_reg(Register::EDX, (old >> 32) as u32 as u64);
    }
    Ok(())
}

fn exec_cmpxchg16b<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    if instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }
    let addr = calc_ea(state, instr, next_ip, true)?;
    if addr & 0xF != 0 {
        return Err(Exception::gp0());
    }

    let expected =
        ((state.read_reg(Register::RDX) as u128) << 64) | (state.read_reg(Register::RAX) as u128);
    let replacement =
        ((state.read_reg(Register::RCX) as u128) << 64) | (state.read_reg(Register::RBX) as u128);

    let (old, swapped) = if instr.has_lock_prefix() {
        if let Some(start) = contiguous_masked_start(state, addr, 16) {
            bus.atomic_rmw::<u128, _>(start, |old| {
                if old == expected {
                    (replacement, (old, true))
                } else {
                    (old, (old, false))
                }
            })?
        } else {
            let old = read_u128_wrapped(state, bus, addr)?;
            if old == expected {
                write_u128_wrapped(state, bus, addr, replacement)?;
                (old, true)
            } else {
                (old, false)
            }
        }
    } else {
        let old = read_u128_wrapped(state, bus, addr)?;
        if old == expected {
            write_u128_wrapped(state, bus, addr, replacement)?;
            (old, true)
        } else {
            (old, false)
        }
    };

    state.set_flag(FLAG_ZF, swapped);
    if !swapped {
        state.write_reg(Register::RAX, old as u64);
        state.write_reg(Register::RDX, (old >> 64) as u64);
    }
    Ok(())
}

fn cmpxchg_acc_reg(bits: u32) -> Result<Register, Exception> {
    match bits {
        8 => Ok(Register::AL),
        16 => Ok(Register::AX),
        32 => Ok(Register::EAX),
        64 => Ok(Register::RAX),
        _ => Err(Exception::InvalidOpcode),
    }
}
