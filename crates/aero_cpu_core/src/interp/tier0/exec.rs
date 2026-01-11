use super::{exec_decoded, ExecOutcome};
use crate::exception::{AssistReason, Exception};
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
    bus.sync(state);
    let ip = state.rip();
    let fetch_addr = state.seg_base_reg(Register::CS).wrapping_add(ip);
    let bytes = match bus.fetch(fetch_addr, 15) {
        Ok(v) => v,
        Err(e) => {
            state.apply_exception_side_effects(&e);
            return Err(e);
        }
    };
    let addr_size_override = has_addr_size_override(&bytes, state.bitness());
    let decoded =
        aero_x86::decode(&bytes, ip, state.bitness()).map_err(|_| Exception::InvalidOpcode)?;
    let next_ip = ip.wrapping_add(decoded.len as u64) & state.mode.ip_mask();
    let outcome = match exec_decoded(state, bus, &decoded, next_ip, addr_size_override) {
        Ok(v) => v,
        Err(e) => {
            state.apply_exception_side_effects(&e);
            return Err(e);
        }
    };
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

fn has_addr_size_override(bytes: &[u8; 15], bitness: u32) -> bool {
    let mut i = 0usize;
    let mut seen = false;
    while i < bytes.len() {
        let b = bytes[i];
        let is_legacy_prefix = matches!(
            b,
            0xF0 | 0xF2 | 0xF3 // lock/rep
                | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 // segment overrides
                | 0x66 // operand-size override
                | 0x67 // address-size override
        );
        let is_rex = bitness == 64 && (0x40..=0x4F).contains(&b);
        if !(is_legacy_prefix || is_rex) {
            break;
        }
        if b == 0x67 {
            seen = true;
        }
        i += 1;
    }
    seen
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
