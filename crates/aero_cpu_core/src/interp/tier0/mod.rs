pub mod exec;

mod ops_alu;
mod ops_cf;
mod ops_data;

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

    match mnem {
        Mnemonic::Hlt => Ok(ExecOutcome::Halt),
        Mnemonic::In | Mnemonic::Out => Ok(ExecOutcome::Assist(AssistReason::Io)),
        Mnemonic::Cpuid => Ok(ExecOutcome::Assist(AssistReason::Cpuid)),
        _ => Err(Exception::InvalidOpcode),
    }
}

