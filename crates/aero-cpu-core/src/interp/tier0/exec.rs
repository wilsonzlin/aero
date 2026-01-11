use super::{exec_decoded, ExecOutcome, Tier0Config};
use crate::assist::{handle_assist_decoded, has_addr_size_override, AssistContext};
use crate::exception::{AssistReason, Exception};
use crate::interrupts::CpuCore;
use crate::linear_mem::fetch_wrapped;
use crate::mem::CpuBus;
use crate::interrupts;
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
    let bytes = match fetch_wrapped(state, bus, fetch_addr, 15) {
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
///
/// Note: this helper operates on [`crate::state::CpuState`] only and therefore
/// cannot emulate interrupt-related assists (`CLI`/`STI`/`INT*`/`IRET*`) which
/// require access to [`crate::interrupts::PendingEventState`]. When it
/// encounters one, it returns [`BatchExit::Assist`] with
/// [`AssistReason::Interrupt`].
pub fn run_batch_with_assists<B: CpuBus>(
    ctx: &mut AssistContext,
    cpu: &mut CpuCore,
    bus: &mut B,
    max_insts: u64,
) -> BatchResult {
    let cfg = Tier0Config::from_cpuid(&ctx.features);
    run_batch_with_assists_with_config(&cfg, ctx, cpu, bus, max_insts)
}

pub fn run_batch_with_assists_with_config<B: CpuBus>(
    cfg: &Tier0Config,
    ctx: &mut AssistContext,
    cpu: &mut CpuCore,
    bus: &mut B,
    max_insts: u64,
) -> BatchResult {
    if cpu.state.halted {
        return BatchResult {
            executed: 0,
            exit: BatchExit::Halted,
        };
    }

    let mut executed = 0u64;
    while executed < max_insts {
        bus.sync(&cpu.state);

        let ip = cpu.state.rip();
        let fetch_addr = cpu
            .state
            .apply_a20(cpu.state.seg_base_reg(Register::CS).wrapping_add(ip));
        let bytes = match fetch_wrapped(&cpu.state, bus, fetch_addr, 15) {
            Ok(bytes) => bytes,
            Err(e) => {
                cpu.state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };

        let addr_size_override = has_addr_size_override(&bytes, cpu.state.bitness());
        let decoded = match aero_x86::decode(&bytes, ip, cpu.state.bitness()) {
            Ok(decoded) => decoded,
            Err(_) => {
                let e = Exception::InvalidOpcode;
                cpu.state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };
        let next_ip_raw = ip.wrapping_add(decoded.len as u64);
        let next_ip = next_ip_raw & cpu.state.mode.ip_mask();

        let outcome = match exec_decoded(
            cfg,
            &mut cpu.state,
            bus,
            &decoded,
            next_ip,
            addr_size_override,
        ) {
            Ok(v) => v,
            Err(e) => {
                if matches!(e, Exception::InvalidOpcode)
                    && is_x87_opcode(&bytes, cpu.state.bitness())
                {
                    if let Err(fp_e) =
                        super::check_fp_available(&mut cpu.state, crate::fpu::FpKind::X87)
                    {
                        cpu.state.apply_exception_side_effects(&fp_e);
                        return BatchResult {
                            executed,
                            exit: BatchExit::Exception(fp_e),
                        };
                    }
                }
                cpu.state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };

        match outcome {
            ExecOutcome::Continue => {
                cpu.state.set_rip(next_ip);
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
            }
            ExecOutcome::ContinueInhibitInterrupts => {
                cpu.state.set_rip(next_ip);
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                cpu.pending.inhibit_interrupts_for_one_instruction();
            }
            ExecOutcome::Branch => {
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                return BatchResult {
                    executed,
                    exit: BatchExit::Branch,
                };
            }
            ExecOutcome::Halt => {
                cpu.state.set_rip(next_ip);
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                if let Some(vector) = cpu.state.take_pending_bios_int() {
                    return BatchResult {
                        executed,
                        exit: BatchExit::BiosInterrupt(vector),
                    };
                }
                cpu.state.halted = true;
                return BatchResult {
                    executed,
                    exit: BatchExit::Halted,
                };
            }
            ExecOutcome::Assist(reason) => {
                // Interrupt-related assists (CLI/STI/INT*/IRET*) require access to
                // `interrupts::PendingEventState`, which is intentionally not part
                // of the public `CpuState` ABI.
                if reason == AssistReason::Interrupt {
                    return BatchResult {
                        executed,
                        exit: BatchExit::Assist(reason),
                    };
                }

                // Execute the instruction via the assist layer using the already decoded form.
                let inhibits_interrupt = matches!(
                    decoded.instr.mnemonic(),
                    aero_x86::Mnemonic::Mov | aero_x86::Mnemonic::Pop
                ) && decoded.instr.op_count() > 0
                    && decoded.instr.op_kind(0) == aero_x86::OpKind::Register
                    && decoded.instr.op0_register() == aero_x86::Register::SS;
                if let Err(e) = handle_assist_decoded(
                    ctx,
                    &mut cpu.time,
                    &mut cpu.state,
                    bus,
                    &decoded,
                    addr_size_override,
                ) {
                    return BatchResult {
                        executed,
                        exit: BatchExit::Exception(e),
                    };
                }
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                if inhibits_interrupt {
                    cpu.pending.inhibit_interrupts_for_one_instruction();
                }

                // Preserve the "basic block" behavior of `run_batch`: treat any
                // control-transfer assist (i.e. RIP != fallthrough) as a branch.
                let expected_next = next_ip_raw & cpu.state.mode.ip_mask();
                if cpu.state.rip() != expected_next {
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

/// Tier-0 batch execution wrapper that resolves assists with access to
/// [`interrupts::CpuCore`] (architectural state + interrupt bookkeeping).
///
/// This is the canonical Tier-0 runner for tests/embeddings that need correct
/// interrupt semantics (`CLI`/`STI`/`INT*`/`IRET*`), including interrupt shadows
/// and IRET frame bookkeeping.
pub fn run_batch_cpu_core_with_assists<B: CpuBus>(
    cfg: &Tier0Config,
    ctx: &mut AssistContext,
    cpu: &mut interrupts::CpuCore,
    bus: &mut B,
    max_insts: u64,
) -> BatchResult {
    use aero_x86::{Mnemonic, OpKind};

    if cpu.state.halted {
        return BatchResult {
            executed: 0,
            exit: BatchExit::Halted,
        };
    }

    let mut executed = 0u64;
    while executed < max_insts {
        bus.sync(&cpu.state);

        let ip = cpu.state.rip();
        let fetch_addr = cpu
            .state
            .apply_a20(cpu.state.seg_base_reg(Register::CS).wrapping_add(ip));
        let bytes = match fetch_wrapped(&cpu.state, bus, fetch_addr, 15) {
            Ok(bytes) => bytes,
            Err(e) => {
                cpu.state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };

        let bitness = cpu.state.bitness();
        let addr_size_override = has_addr_size_override(&bytes, bitness);
        let decoded = match aero_x86::decode(&bytes, ip, bitness) {
            Ok(decoded) => decoded,
            Err(_) => {
                let e = Exception::InvalidOpcode;
                cpu.state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };
        let next_ip_raw = ip.wrapping_add(decoded.len as u64);
        let next_ip = next_ip_raw & cpu.state.mode.ip_mask();

        let outcome = match exec_decoded(
            cfg,
            &mut cpu.state,
            bus,
            &decoded,
            next_ip,
            addr_size_override,
        ) {
            Ok(v) => v,
            Err(e) => {
                if matches!(e, Exception::InvalidOpcode) && is_x87_opcode(&bytes, bitness) {
                    if let Err(fp_e) = super::check_fp_available(&cpu.state, crate::fpu::FpKind::X87)
                    {
                        cpu.state.apply_exception_side_effects(&fp_e);
                        return BatchResult {
                            executed,
                            exit: BatchExit::Exception(fp_e),
                        };
                    }
                }
                cpu.state.apply_exception_side_effects(&e);
                return BatchResult {
                    executed,
                    exit: BatchExit::Exception(e),
                };
            }
        };

        match outcome {
            ExecOutcome::Continue => {
                cpu.state.set_rip(next_ip);
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
            }
            ExecOutcome::ContinueInhibitInterrupts => {
                cpu.state.set_rip(next_ip);
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                cpu.pending.inhibit_interrupts_for_one_instruction();
            }
            ExecOutcome::Branch => {
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                return BatchResult {
                    executed,
                    exit: BatchExit::Branch,
                };
            }
            ExecOutcome::Halt => {
                cpu.state.set_rip(next_ip);
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                if let Some(vector) = cpu.state.take_pending_bios_int() {
                    return BatchResult {
                        executed,
                        exit: BatchExit::BiosInterrupt(vector),
                    };
                }
                cpu.state.halted = true;
                return BatchResult {
                    executed,
                    exit: BatchExit::Halted,
                };
            }
            ExecOutcome::Assist(reason) => {
                if reason == AssistReason::Interrupt {
                    let assist_outcome = interrupts::exec_interrupt_assist_decoded(
                        cpu,
                        bus,
                        &decoded,
                        addr_size_override,
                    )
                    .unwrap_or_else(|e| panic!("interrupt delivery failed: {e:?}"));

                    match assist_outcome {
                        interrupts::InterruptAssistOutcome::Retired {
                            block_boundary,
                            inhibit_interrupts,
                        } => {
                            executed += 1;
                            cpu.pending.retire_instruction();
                            cpu.time.advance_cycles(1);
                            cpu.state.msr.tsc = cpu.time.read_tsc();
                            if inhibit_interrupts {
                                cpu.pending.inhibit_interrupts_for_one_instruction();
                            }
                            if block_boundary {
                                return BatchResult {
                                    executed,
                                    exit: BatchExit::Branch,
                                };
                            }
                            continue;
                        }
                        interrupts::InterruptAssistOutcome::FaultDelivered => {
                            return BatchResult {
                                executed,
                                exit: BatchExit::Branch,
                            };
                        }
                    }
                }

                let inhibits_interrupt = matches!(
                    decoded.instr.mnemonic(),
                    Mnemonic::Mov | Mnemonic::Pop
                ) && decoded.instr.op_count() > 0
                    && decoded.instr.op_kind(0) == OpKind::Register
                    && decoded.instr.op0_register() == Register::SS;

                if let Err(e) = handle_assist_decoded(
                    ctx,
                    &mut cpu.time,
                    &mut cpu.state,
                    bus,
                    &decoded,
                    addr_size_override,
                ) {
                    return BatchResult {
                        executed,
                        exit: BatchExit::Exception(e),
                    };
                }
                executed += 1;
                cpu.pending.retire_instruction();
                cpu.time.advance_cycles(1);
                cpu.state.msr.tsc = cpu.time.read_tsc();
                if inhibits_interrupt {
                    cpu.pending.inhibit_interrupts_for_one_instruction();
                }

                // Preserve the "basic block" behavior of `run_batch`: treat any
                // control-transfer assist (i.e. RIP != fallthrough) as a branch.
                let expected_next = next_ip_raw & cpu.state.mode.ip_mask();
                if cpu.state.rip() != expected_next {
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
