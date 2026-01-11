use super::ops_data::{calc_ea, op_bits};
use super::ExecOutcome;
use crate::exception::Exception;
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
        Mnemonic::Cmpxchg => exec_cmpxchg(state, bus, instr, next_ip),
        Mnemonic::Cmpxchg8b => exec_cmpxchg8b(state, bus, instr, next_ip),
        Mnemonic::Cmpxchg16b => exec_cmpxchg16b(state, bus, instr, next_ip),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn acc_reg(bits: u32) -> Result<Register, Exception> {
    match bits {
        8 => Ok(Register::AL),
        16 => Ok(Register::AX),
        32 => Ok(Register::EAX),
        64 => Ok(Register::RAX),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_cmpxchg<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let bits = op_bits(state, instr, 0)?;
    let mask = mask_bits(bits);
    let expected_reg = acc_reg(bits)?;
    let expected = state.read_reg(expected_reg) & mask;
    let src = state.read_reg(instr.op1_register()) & mask;
    let lock = instr.has_lock_prefix();

    match instr.op_kind(0) {
        OpKind::Register => {
            if lock {
                return Err(Exception::InvalidOpcode);
            }
            let dst_reg = instr.op0_register();
            let dst = state.read_reg(dst_reg) & mask;
            let (_, flags) = super::ops_alu::sub_with_flags(state, expected, dst, 0, bits);
            state.set_rflags(flags);
            if dst == expected {
                state.write_reg(dst_reg, src);
            } else {
                state.write_reg(expected_reg, dst);
            }
            Ok(ExecOutcome::Continue)
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            if lock {
                let (old, swapped) = super::atomic_rmw_sized(bus, addr, bits, |old| {
                    let old = old & mask;
                    if old == expected {
                        (src, (old, true))
                    } else {
                        (old, (old, false))
                    }
                })?;
                let (_, flags) = super::ops_alu::sub_with_flags(state, expected, old, 0, bits);
                state.set_rflags(flags);
                if !swapped {
                    state.write_reg(expected_reg, old);
                }
            } else {
                let old = super::ops_data::read_mem(bus, addr, bits)? & mask;
                let (_, flags) = super::ops_alu::sub_with_flags(state, expected, old, 0, bits);
                state.set_rflags(flags);
                if old == expected {
                    super::ops_data::write_mem(bus, addr, bits, src)?;
                } else {
                    state.write_reg(expected_reg, old);
                }
            }
            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_cmpxchg8b<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    if instr.op_count() != 1 || instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }

    let addr = calc_ea(state, instr, next_ip, true)?;
    let expected = ((state.read_reg(Register::EDX) as u64) << 32)
        | (state.read_reg(Register::EAX) as u32 as u64);
    let replacement = ((state.read_reg(Register::ECX) as u64) << 32)
        | (state.read_reg(Register::EBX) as u32 as u64);
    let lock = instr.has_lock_prefix();

    let (old, swapped) = if lock {
        bus.atomic_rmw::<u64, _>(addr, |old| {
            if old == expected {
                (replacement, (old, true))
            } else {
                (old, (old, false))
            }
        })?
    } else {
        let old = bus.read_u64(addr)?;
        if old == expected {
            bus.write_u64(addr, replacement)?;
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
    Ok(ExecOutcome::Continue)
}

fn exec_cmpxchg16b<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    if instr.op_count() != 1 || instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }

    let addr = calc_ea(state, instr, next_ip, true)?;
    if addr & 0xF != 0 {
        return Err(Exception::gp0());
    }

    let expected =
        ((state.read_reg(Register::RDX) as u128) << 64) | state.read_reg(Register::RAX) as u128;
    let replacement =
        ((state.read_reg(Register::RCX) as u128) << 64) | state.read_reg(Register::RBX) as u128;
    let lock = instr.has_lock_prefix();

    let (old, swapped) = if lock {
        bus.atomic_rmw::<u128, _>(addr, |old| {
            if old == expected {
                (replacement, (old, true))
            } else {
                (old, (old, false))
            }
        })?
    } else {
        let old = bus.read_u128(addr)?;
        if old == expected {
            bus.write_u128(addr, replacement)?;
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
    Ok(ExecOutcome::Continue)
}
