pub mod exec;

mod ops_atomic;
mod ops_alu;
mod ops_cf;
mod ops_data;
mod ops_string;
mod ops_x87;

use crate::exception::{AssistReason, Exception};
use crate::mem::CpuBus;
use crate::state::CpuState;
use aero_x86::{DecodedInst, Mnemonic};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecOutcome {
    Continue,
    Branch,
    Halt,
    Assist(AssistReason),
}

fn atomic_rmw_sized<B: CpuBus, R>(
    bus: &mut B,
    addr: u64,
    bits: u32,
    f: impl FnOnce(u64) -> (u64, R),
) -> Result<R, Exception> {
    match bits {
        8 => bus.atomic_rmw::<u8, _>(addr, |old| {
            let (new, ret) = f(old as u64);
            (new as u8, ret)
        }),
        16 => bus.atomic_rmw::<u16, _>(addr, |old| {
            let (new, ret) = f(old as u64);
            (new as u16, ret)
        }),
        32 => bus.atomic_rmw::<u32, _>(addr, |old| {
            let (new, ret) = f(old as u64);
            (new as u32, ret)
        }),
        64 => bus.atomic_rmw::<u64, _>(addr, |old| f(old)),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_decoded<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
    addr_size_override: bool,
) -> Result<ExecOutcome, Exception> {
    let mnem = decoded.instr.mnemonic();
    if ops_cf::handles_mnemonic(mnem) {
        return ops_cf::exec(state, bus, decoded, next_ip);
    }
    if ops_atomic::handles_mnemonic(mnem) {
        return ops_atomic::exec(state, bus, decoded, next_ip);
    }
    if ops_data::handles_mnemonic(mnem) {
        return ops_data::exec(state, bus, decoded, next_ip);
    }
    if ops_alu::handles_mnemonic(mnem) {
        return ops_alu::exec(state, bus, decoded, next_ip);
    }
    if ops_x87::handles_mnemonic(mnem) {
        return ops_x87::exec(state, bus, decoded, next_ip);
    }
    if ops_string::handles(&decoded.instr) {
        return ops_string::exec(state, bus, decoded, next_ip, addr_size_override);
    }

    match mnem {
        Mnemonic::Hlt => Ok(ExecOutcome::Halt),
        Mnemonic::In
        | Mnemonic::Out
        | Mnemonic::Insb
        | Mnemonic::Insw
        | Mnemonic::Insd
        | Mnemonic::Outsb
        | Mnemonic::Outsw
        | Mnemonic::Outsd => Ok(ExecOutcome::Assist(AssistReason::Io)),
        Mnemonic::Cpuid => Ok(ExecOutcome::Assist(AssistReason::Cpuid)),
        Mnemonic::Rdmsr | Mnemonic::Wrmsr => Ok(ExecOutcome::Assist(AssistReason::Msr)),
        Mnemonic::Int | Mnemonic::Int1 | Mnemonic::Int3 | Mnemonic::Into => {
            Ok(ExecOutcome::Assist(AssistReason::Interrupt))
        }
        Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq => {
            Ok(ExecOutcome::Assist(AssistReason::Interrupt))
        }
        Mnemonic::Cli | Mnemonic::Sti => Ok(ExecOutcome::Assist(AssistReason::Interrupt)),
        // Privileged/system instructions that require additional CPU core state.
        Mnemonic::Lgdt
        | Mnemonic::Lidt
        | Mnemonic::Sgdt
        | Mnemonic::Sidt
        | Mnemonic::Ltr
        | Mnemonic::Str
        | Mnemonic::Lldt
        | Mnemonic::Sldt
        | Mnemonic::Lmsw
        | Mnemonic::Smsw
        | Mnemonic::Invlpg
        | Mnemonic::Swapgs
        | Mnemonic::Syscall
        | Mnemonic::Sysret
        | Mnemonic::Sysenter
        | Mnemonic::Sysexit
        | Mnemonic::Rsm => Ok(ExecOutcome::Assist(AssistReason::Privileged)),
        Mnemonic::Rdtsc
        | Mnemonic::Rdtscp
        | Mnemonic::Lfence
        | Mnemonic::Sfence
        | Mnemonic::Mfence => Ok(ExecOutcome::Assist(AssistReason::Unsupported)),
        _ => Err(Exception::InvalidOpcode),
    }
}
