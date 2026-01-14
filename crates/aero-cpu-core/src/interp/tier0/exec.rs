use super::{exec_decoded, ExecOutcome, Tier0Config};
use crate::assist::{handle_assist_decoded, has_addr_size_override, AssistContext};
use crate::exception::{AssistReason, Exception};
use crate::interrupts;
use crate::interrupts::CpuCore;
use crate::linear_mem::fetch_wrapped_seg_ip;
use crate::mem::CpuBus;
use crate::state::{mask_bits, CpuState};
use aero_x86::Register;

#[derive(Debug, Clone)]
pub enum StepExit {
    Continue,
    /// The instruction completed normally, and maskable interrupts should be
    /// inhibited for exactly one subsequent instruction (MOV SS / POP SS shadow).
    ContinueInhibitInterrupts,
    Branch,
    Halted,
    BiosInterrupt(u8),
    /// Tier-0 could decode the instruction but does not implement its semantics
    /// and wants the caller to emulate it via the assist layer.
    ///
    /// The faulting instruction has already been fetched + decoded, so callers
    /// can avoid a second fetch/decode pass by using [`crate::assist::handle_assist_decoded`]
    /// (or [`crate::interrupts::exec_interrupt_assist_decoded`] for interrupt-related
    /// instructions).
    Assist {
        reason: AssistReason,
        decoded: aero_x86::DecodedInst,
        /// Whether an address-size override prefix (0x67) was present. This is
        /// tracked separately because Tier-0 currently only uses `iced-x86`'s
        /// decoded instruction, which does not expose a simple "was 0x67 seen"
        /// query that matches our string-op/IO assist needs.
        addr_size_override: bool,
    },
}

impl PartialEq for StepExit {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Continue, Self::Continue)
            | (Self::ContinueInhibitInterrupts, Self::ContinueInhibitInterrupts)
            | (Self::Branch, Self::Branch)
            | (Self::Halted, Self::Halted) => true,
            (Self::BiosInterrupt(a), Self::BiosInterrupt(b)) => a == b,
            (Self::Assist { reason: a, .. }, Self::Assist { reason: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Eq for StepExit {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchExit {
    Completed,
    Branch,
    Halted,
    BiosInterrupt(u8),
    Assist(AssistReason),
    Exception(Exception),
    CpuExit(interrupts::CpuExit),
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
    step_with_config_and_decoder(cfg, state, bus, |bytes, ip, bitness| {
        aero_x86::decode(bytes, ip, bitness)
    })
}

pub fn step<B: CpuBus>(state: &mut CpuState, bus: &mut B) -> Result<StepExit, Exception> {
    let cfg = Tier0Config::default();
    step_with_config(&cfg, state, bus)
}

pub(crate) fn step_with_config_and_decoder<B, F>(
    cfg: &Tier0Config,
    state: &mut CpuState,
    bus: &mut B,
    mut decode: F,
) -> Result<StepExit, Exception>
where
    B: CpuBus,
    F: FnMut(&[u8; 15], u64, u32) -> Result<aero_x86::DecodedInst, aero_x86::DecodeError>,
{
    bus.sync(state);

    let ip = state.rip();
    let cs_base = state.seg_base_reg(Register::CS);
    let bytes = match fetch_wrapped_seg_ip(state, bus, cs_base, ip, 15) {
        Ok(v) => v,
        Err(e) => {
            state.apply_exception_side_effects(&e);
            return Err(e);
        }
    };
    let bitness = state.bitness();
    let addr_size_override = has_addr_size_override(&bytes, bitness);
    let decoded = decode(&bytes, ip, bitness).map_err(|_| Exception::InvalidOpcode)?;
    let next_ip = ip.wrapping_add(decoded.len as u64) & mask_bits(bitness);

    let outcome = match exec_decoded(cfg, state, bus, &decoded, next_ip, addr_size_override) {
        Ok(v) => v,
        Err(e) => {
            // x87 opcodes (D8-DF, optionally preceded by FWAIT=9B) should still obey CR0.EM/TS
            // gating even if Tier-0 doesn't implement the specific mnemonic yet.
            if matches!(e, Exception::InvalidOpcode) && is_x87_opcode(&bytes, bitness) {
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
        ExecOutcome::Assist(reason) => Ok(StepExit::Assist {
            reason,
            decoded,
            addr_size_override,
        }),
    }
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
            Ok(StepExit::Assist { reason, .. }) => {
                return BatchResult {
                    executed,
                    exit: BatchExit::Assist(reason),
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
/// Note: this helper intentionally does not execute interrupt-related assists
/// (`CLI`/`STI`/`INT*`/`IRET*`). Those instructions require the architectural
/// interrupt engine in [`crate::interrupts`] (including interrupt shadow and
/// IRET bookkeeping). When an interrupt assist is encountered, this helper
/// returns [`BatchExit::Assist`] with [`AssistReason::Interrupt`].
///
/// This helper will still deliver any already-queued events in
/// [`crate::interrupts::PendingEventState`] (pending exceptions + external
/// interrupt FIFO) at instruction boundaries, including waking the CPU from
/// `HLT` when a maskable interrupt is delivered.
///
/// If event delivery fails (e.g. triple fault), this helper returns
/// [`BatchExit::CpuExit`].
///
/// Use [`run_batch_cpu_core_with_assists`] if you need Tier-0 to resolve
/// interrupt assists.
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
    if max_insts == 0 {
        return BatchResult {
            executed: 0,
            exit: if cpu.state.halted {
                BatchExit::Halted
            } else {
                BatchExit::Completed
            },
        };
    }

    let mut executed = 0u64;
    while executed < max_insts {
        // Give pending exceptions/interrupts a chance at instruction boundaries.
        if cpu.pending.has_pending_event() {
            match cpu.deliver_pending_event(bus) {
                Ok(()) => continue,
                Err(exit) => {
                    return BatchResult {
                        executed,
                        exit: BatchExit::CpuExit(exit),
                    };
                }
            }
        }
        if !cpu.pending.external_interrupts().is_empty() {
            let before = cpu.pending.external_interrupts().len();
            match cpu.deliver_external_interrupt(bus) {
                Ok(()) => {
                    if cpu.pending.external_interrupts().len() != before {
                        continue;
                    }
                }
                Err(exit) => {
                    return BatchResult {
                        executed,
                        exit: BatchExit::CpuExit(exit),
                    };
                }
            }
        }
        if cpu.state.halted {
            return BatchResult {
                executed,
                exit: BatchExit::Halted,
            };
        }

        bus.sync(&cpu.state);

        let ip = cpu.state.rip();
        let cs_base = cpu.state.seg_base_reg(Register::CS);
        let bytes = match fetch_wrapped_seg_ip(&cpu.state, bus, cs_base, ip, 15) {
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
        let next_ip = next_ip_raw & mask_bits(cpu.state.bitness());

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
                        super::check_fp_available(&cpu.state, crate::fpu::FpKind::X87)
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
                let expected_next = next_ip_raw & mask_bits(cpu.state.bitness());
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
///
/// Like [`crate::exec::Vcpu::maybe_deliver_interrupt`], this helper also gives
/// any already-queued exceptions/interrupts in [`interrupts::PendingEventState`]
/// a chance at instruction boundaries.
///
/// If event delivery fails (e.g. triple fault), this helper returns
/// [`BatchExit::CpuExit`].
pub fn run_batch_cpu_core_with_assists<B: CpuBus>(
    cfg: &Tier0Config,
    ctx: &mut AssistContext,
    cpu: &mut interrupts::CpuCore,
    bus: &mut B,
    max_insts: u64,
) -> BatchResult {
    use aero_x86::{Mnemonic, OpKind};

    if max_insts == 0 {
        return BatchResult {
            executed: 0,
            exit: if cpu.state.halted {
                BatchExit::Halted
            } else {
                BatchExit::Completed
            },
        };
    }

    let mut executed = 0u64;
    while executed < max_insts {
        // Give pending exceptions/interrupts a chance at instruction boundaries.
        if cpu.pending.has_pending_event() {
            match cpu.deliver_pending_event(bus) {
                Ok(()) => continue,
                Err(exit) => {
                    return BatchResult {
                        executed,
                        exit: BatchExit::CpuExit(exit),
                    };
                }
            }
        }
        if !cpu.pending.external_interrupts().is_empty() {
            let before = cpu.pending.external_interrupts().len();
            match cpu.deliver_external_interrupt(bus) {
                Ok(()) => {
                    if cpu.pending.external_interrupts().len() != before {
                        continue;
                    }
                }
                Err(exit) => {
                    return BatchResult {
                        executed,
                        exit: BatchExit::CpuExit(exit),
                    };
                }
            }
        }
        if cpu.state.halted {
            return BatchResult {
                executed,
                exit: BatchExit::Halted,
            };
        }

        bus.sync(&cpu.state);

        let ip = cpu.state.rip();
        let cs_base = cpu.state.seg_base_reg(Register::CS);
        let bytes = match fetch_wrapped_seg_ip(&cpu.state, bus, cs_base, ip, 15) {
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
        let next_ip = next_ip_raw & mask_bits(cpu.state.bitness());

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
                    if let Err(fp_e) =
                        super::check_fp_available(&cpu.state, crate::fpu::FpKind::X87)
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
                    let assist_outcome = match interrupts::exec_interrupt_assist_decoded(
                        cpu,
                        bus,
                        &decoded,
                        addr_size_override,
                    ) {
                        Ok(outcome) => outcome,
                        Err(exit) => {
                            return BatchResult {
                                executed,
                                exit: BatchExit::CpuExit(exit),
                            };
                        }
                    };

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

                let inhibits_interrupt =
                    matches!(decoded.instr.mnemonic(), Mnemonic::Mov | Mnemonic::Pop)
                        && decoded.instr.op_count() > 0
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
                let expected_next = next_ip_raw & mask_bits(cpu.state.bitness());
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
