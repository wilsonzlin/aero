use crate::exception::{AssistReason, Exception};
use super::{exec_decoded, ExecOutcome};
use crate::mem::CpuBus;
use crate::state::CpuState;
use aero_x86::Register;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepExit {
    Continue,
    Branch,
    Halted,
    Assist(AssistReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchExit {
    Completed,
    Branch,
    Halted,
    Assist(AssistReason),
    Exception(Exception),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchResult {
    pub executed: u64,
    pub exit: BatchExit,
}

pub fn step<B: CpuBus>(state: &mut CpuState, bus: &mut B) -> Result<StepExit, Exception> {
    let ip = state.rip();
    let fetch_addr = state.seg_base_reg(Register::CS).wrapping_add(ip);
    let bytes = bus.fetch(fetch_addr, 15)?;
    let decoded =
        aero_x86::decode(&bytes, ip, state.bitness()).map_err(|_| Exception::InvalidOpcode)?;
    let next_ip = ip.wrapping_add(decoded.len as u64) & state.mode.ip_mask();
    let outcome = exec_decoded(state, bus, &decoded, next_ip)?;
    match outcome {
        ExecOutcome::Continue => {
            state.set_rip(next_ip);
            Ok(StepExit::Continue)
        }
        ExecOutcome::Halt => {
            state.set_rip(next_ip);
            state.halted = true;
            Ok(StepExit::Halted)
        }
        ExecOutcome::Branch => Ok(StepExit::Branch),
        ExecOutcome::Assist(r) => Ok(StepExit::Assist(r)),
    }
}

pub fn run_batch<B: CpuBus>(state: &mut CpuState, bus: &mut B, max_insts: u64) -> BatchResult {
    if state.halted {
        return BatchResult {
            executed: 0,
            exit: BatchExit::Halted,
        };
    }

    let mut executed = 0u64;
    while executed < max_insts {
        match step(state, bus) {
            Ok(StepExit::Continue) => executed += 1,
            Ok(StepExit::Branch) => {
                executed += 1;
                return BatchResult {
                    executed,
                    exit: BatchExit::Branch,
                };
            }
            Ok(StepExit::Halted) => {
                executed += 1;
                return BatchResult {
                    executed,
                    exit: BatchExit::Halted,
                };
            }
            Ok(StepExit::Assist(r)) => {
                return BatchResult {
                    executed,
                    exit: BatchExit::Assist(r),
                };
            }
            Err(e) => {
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        }
    }

    BatchResult {
        executed,
        exit: BatchExit::Completed,
    }
}
