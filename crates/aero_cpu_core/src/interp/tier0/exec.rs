use super::{exec_decoded, ExecOutcome, Tier0Config};
use crate::assist::{handle_assist_decoded, AssistContext};
use crate::exception::{AssistReason, Exception};
use crate::mem::CpuBus;
use crate::state::CpuState;
use aero_x86::Register;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepExit {
    Continue,
    /// The instruction completed normally, and maskable interrupts should be
    /// inhibited for exactly one subsequent instruction (MOV SS / POP SS shadow).
    ContinueInhibitInterrupts,
    Branch,
    Halted,
    BiosInterrupt(u8),
    Assist(AssistReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchExit {
    Completed,
    Branch,
    Halted,
    BiosInterrupt(u8),
    Assist(AssistReason),
    Exception(Exception),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchResult {
    pub executed: u64,
    pub exit: BatchExit,
}

pub fn step_with_config<B: CpuBus>(
    cfg: &Tier0Config,
    state: &mut CpuState,
    bus: &mut B,
) -> Result<StepExit, Exception> {
    bus.sync(state);

    let ip = state.rip();
    let fetch_addr = state.apply_a20(state.seg_base_reg(Register::CS).wrapping_add(ip));
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

    let outcome = match exec_decoded(cfg, state, bus, &decoded, next_ip, addr_size_override) {
        Ok(v) => v,
        Err(e) => {
            // x87 opcodes (D8-DF, optionally preceded by FWAIT=9B) should still obey CR0.EM/TS
            // gating even if Tier-0 doesn't implement the specific mnemonic yet.
            if matches!(e, Exception::InvalidOpcode) && is_x87_opcode(&bytes, state.bitness()) {
                if let Err(fp_e) = super::check_fp_available(state, crate::fpu::FpKind::X87) {
                    state.apply_exception_side_effects(&fp_e);
                    return Err(fp_e);
                }
            }

            state.apply_exception_side_effects(&e);
            return Err(e);
        }
    };

    match outcome {
        ExecOutcome::Continue => {
            state.set_rip(next_ip);
            Ok(StepExit::Continue)
        }
        ExecOutcome::ContinueInhibitInterrupts => {
            state.set_rip(next_ip);
            Ok(StepExit::ContinueInhibitInterrupts)
        }
        ExecOutcome::Halt => {
            state.set_rip(next_ip);
            if let Some(vector) = state.take_pending_bios_int() {
                Ok(StepExit::BiosInterrupt(vector))
            } else {
                state.halted = true;
                Ok(StepExit::Halted)
            }
        }
        ExecOutcome::Branch => Ok(StepExit::Branch),
        ExecOutcome::Assist(r) => Ok(StepExit::Assist(r)),
    }
}

pub fn step<B: CpuBus>(state: &mut CpuState, bus: &mut B) -> Result<StepExit, Exception> {
    let cfg = Tier0Config::default();
    step_with_config(&cfg, state, bus)
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

fn is_x87_opcode(bytes: &[u8; 15], bitness: u32) -> bool {
    // Skip legacy prefixes + REX to find the first opcode byte.
    let mut i = 0usize;
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
        i += 1;
    }

    if i >= bytes.len() {
        return false;
    }

    match bytes[i] {
        0xD8..=0xDF => true,
        0x9B => {
            // Many x87 "wait" forms are encoded with an FWAIT prefix (9B) followed by an x87 opcode.
            i + 1 < bytes.len() && matches!(bytes[i + 1], 0xD8..=0xDF)
        }
        _ => false,
    }
}

pub fn run_batch_with_config<B: CpuBus>(
    cfg: &Tier0Config,
    state: &mut CpuState,
    bus: &mut B,
    max_insts: u64,
) -> BatchResult {
    if state.halted {
        return BatchResult {
            executed: 0,
            exit: BatchExit::Halted,
        };
    }

    let mut executed = 0u64;
    while executed < max_insts {
        match step_with_config(cfg, state, bus) {
            Ok(StepExit::Continue) => executed += 1,
            Ok(StepExit::ContinueInhibitInterrupts) => executed += 1,
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
            Ok(StepExit::BiosInterrupt(vector)) => {
                executed += 1;
                return BatchResult {
                    executed,
                    exit: BatchExit::BiosInterrupt(vector),
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

pub fn run_batch<B: CpuBus>(state: &mut CpuState, bus: &mut B, max_insts: u64) -> BatchResult {
    let cfg = Tier0Config::default();
    run_batch_with_config(&cfg, state, bus, max_insts)
}

/// Tier-0 batch execution wrapper that resolves [`StepExit::Assist`] exits via
/// the [`crate::assist`] module.
///
/// This keeps the core Tier-0 interpreter minimal while still allowing it to
/// execute privileged/IO/time instructions required by OS boot code.
pub fn run_batch_with_assists<B: CpuBus>(
    ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    max_insts: u64,
) -> BatchResult {
    let cfg = Tier0Config::default();
    run_batch_with_assists_with_config(&cfg, ctx, state, bus, max_insts)
}

pub fn run_batch_with_assists_with_config<B: CpuBus>(
    cfg: &Tier0Config,
    ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    max_insts: u64,
) -> BatchResult {
    if state.halted {
        return BatchResult {
            executed: 0,
            exit: BatchExit::Halted,
        };
    }

    let mut executed = 0u64;
    while executed < max_insts {
        bus.sync(state);

        let ip = state.rip();
        let fetch_addr = state.apply_a20(state.seg_base_reg(Register::CS).wrapping_add(ip));
        let bytes = match bus.fetch(fetch_addr, 15) {
            Ok(bytes) => bytes,
            Err(e) => {
                state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };

        let addr_size_override = has_addr_size_override(&bytes, state.bitness());
        let decoded = match aero_x86::decode(&bytes, ip, state.bitness()) {
            Ok(decoded) => decoded,
            Err(_) => {
                let e = Exception::InvalidOpcode;
                state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };
        let next_ip_raw = ip.wrapping_add(decoded.len as u64);
        let next_ip = next_ip_raw & state.mode.ip_mask();

        let outcome = match exec_decoded(cfg, state, bus, &decoded, next_ip, addr_size_override) {
            Ok(v) => v,
            Err(e) => {
                if matches!(e, Exception::InvalidOpcode) && is_x87_opcode(&bytes, state.bitness()) {
                    if let Err(fp_e) = super::check_fp_available(state, crate::fpu::FpKind::X87) {
                        state.apply_exception_side_effects(&fp_e);
                        return BatchResult {
                            executed,
                            exit: BatchExit::Exception(fp_e),
                        };
                    }
                }
                state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };

        match outcome {
            ExecOutcome::Continue => {
                state.set_rip(next_ip);
                executed += 1;
            }
            ExecOutcome::ContinueInhibitInterrupts => {
                state.set_rip(next_ip);
                executed += 1;
            }
            ExecOutcome::Branch => {
                executed += 1;
                return BatchResult {
                    executed,
                    exit: BatchExit::Branch,
                };
            }
            ExecOutcome::Halt => {
                state.set_rip(next_ip);
                executed += 1;
                if let Some(vector) = state.take_pending_bios_int() {
                    return BatchResult {
                        executed,
                        exit: BatchExit::BiosInterrupt(vector),
                    };
                }
                state.halted = true;
                return BatchResult {
                    executed,
                    exit: BatchExit::Halted,
                };
            }
            ExecOutcome::Assist(_reason) => {
                // Execute the instruction via the assist layer using the already decoded form.
                if let Err(e) = handle_assist_decoded(ctx, state, bus, &decoded, addr_size_override) {
                    return BatchResult {
                        executed,
                        exit: BatchExit::Exception(e),
                    };
                }
                executed += 1;

                // Preserve the "basic block" behavior of `run_batch`: treat any
                // control-transfer assist (i.e. RIP != fallthrough) as a branch.
                let expected_next = next_ip_raw & state.mode.ip_mask();
                if state.rip() != expected_next {
                    return BatchResult {
                        executed,
                        exit: BatchExit::Branch,
                    };
                }
            }
        }
    }

    BatchResult {
        executed,
        exit: BatchExit::Completed,
    }
}
