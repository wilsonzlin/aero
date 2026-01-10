pub mod exec;

mod ops_alu;
mod ops_cf;
mod ops_data;
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

fn exec_decoded<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let mnem = decoded.instr.mnemonic();
    if ops_cf::handles_mnemonic(mnem) {
        return ops_cf::exec(state, bus, decoded, next_ip);
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
        Mnemonic::Rdtsc | Mnemonic::Rdtscp => Ok(ExecOutcome::Assist(AssistReason::Unsupported)),
        _ => Err(Exception::InvalidOpcode),
    }
}
