use crate::assist::{handle_assist_decoded, has_addr_size_override, AssistContext};
use crate::jit::runtime::{CompileRequestSink, JitBackend, JitBlockExit, JitRuntime};
use crate::linear_mem::fetch_wrapped;

mod exception_bridge;

pub trait ExecCpu {
    fn rip(&self) -> u64;
    fn set_rip(&mut self, rip: u64);
    fn maybe_deliver_interrupt(&mut self) -> bool;
    /// Called by the tiered execution dispatcher when a block of guest instructions retires.
    ///
    /// Tier-0 (interpreter) backends typically perform per-instruction retirement bookkeeping
    /// directly. Tier-1+ JIT backends may retire multiple guest instructions at once and need a
    /// generic way to request the same architectural bookkeeping without knowing the concrete CPU
    /// type.
    ///
    /// Default implementation is a no-op so non-core CPU models used in unit tests remain
    /// source-compatible.
    fn on_retire_instructions(&mut self, _instructions: u64, _inhibit_interrupts: bool) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterpreterBlockExit {
    pub next_rip: u64,
    pub instructions_retired: u64,
}

pub trait Interpreter<Cpu: ExecCpu> {
    fn exec_block(&mut self, cpu: &mut Cpu) -> InterpreterBlockExit;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutedTier {
    Interpreter,
    Jit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    InterruptDelivered,
    Block {
        tier: ExecutedTier,
        entry_rip: u64,
        next_rip: u64,
        instructions_retired: u64,
    },
}

pub struct ExecDispatcher<I, B, C> {
    interpreter: I,
    jit: JitRuntime<B, C>,
    force_interpreter: bool,
}

impl<I, B, C> ExecDispatcher<I, B, C>
where
    B: JitBackend,
    B::Cpu: ExecCpu,
    I: Interpreter<B::Cpu>,
    C: CompileRequestSink,
{
    pub fn new(interpreter: I, jit: JitRuntime<B, C>) -> Self {
        Self {
            interpreter,
            jit,
            force_interpreter: false,
        }
    }

    pub fn jit_mut(&mut self) -> &mut JitRuntime<B, C> {
        &mut self.jit
    }

    pub fn step(&mut self, cpu: &mut B::Cpu) -> StepOutcome {
        if cpu.maybe_deliver_interrupt() {
            return StepOutcome::InterruptDelivered;
        }

        let entry_rip = cpu.rip();
        let compiled = self.jit.prepare_block(entry_rip);

        if self.force_interpreter || compiled.is_none() {
            let exit = self.interpreter.exec_block(cpu);
            cpu.set_rip(exit.next_rip);
            self.force_interpreter = false;
            return StepOutcome::Block {
                tier: ExecutedTier::Interpreter,
                entry_rip,
                next_rip: exit.next_rip,
                instructions_retired: exit.instructions_retired,
            };
        }

        let handle = compiled.expect("checked is_some above");
        let exit: JitBlockExit = self.jit.execute_block(cpu, &handle);
        if exit.committed {
            let retired = u64::from(handle.meta.instruction_count);
            cpu.on_retire_instructions(retired, handle.meta.inhibit_interrupts_after_block);
        }
        cpu.set_rip(exit.next_rip);
        self.force_interpreter = exit.exit_to_interpreter;

        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            entry_rip,
            next_rip: exit.next_rip,
            instructions_retired: if exit.committed {
                u64::from(handle.meta.instruction_count)
            } else {
                0
            },
        }
    }

    pub fn run_blocks(&mut self, cpu: &mut B::Cpu, mut blocks: u64) {
        while blocks > 0 {
            match self.step(cpu) {
                StepOutcome::InterruptDelivered => continue,
                StepOutcome::Block { .. } => blocks -= 1,
            }
        }
    }
}

// ---- Tier-0 glue ------------------------------------------------------------

/// A simple vCPU wrapper that bundles the Tier-0/JIT [`crate::state::CpuState`],
/// interrupt bookkeeping (`CpuCore`), and a memory bus implementation.
///
/// This provides an [`ExecCpu`] implementation suitable for driving the tiered
/// dispatcher (`ExecDispatcher`): [`ExecCpu::maybe_deliver_interrupt`] uses the
/// architectural interrupt delivery logic in [`crate::interrupts`].
#[derive(Debug)]
pub struct Vcpu<B: crate::mem::CpuBus> {
    pub cpu: crate::interrupts::CpuCore,
    pub bus: B,
    /// Sticky CPU exit status (e.g. triple fault, memory fault) observed during execution.
    pub exit: Option<crate::interrupts::CpuExit>,
}

impl<B: crate::mem::CpuBus> Vcpu<B> {
    pub fn new(cpu: crate::interrupts::CpuCore, bus: B) -> Self {
        Self {
            cpu,
            bus,
            exit: None,
        }
    }

    pub fn new_with_mode(mode: crate::state::CpuMode, bus: B) -> Self {
        Self::new(crate::interrupts::CpuCore::new(mode), bus)
    }
}

impl<B: crate::mem::CpuBus> ExecCpu for Vcpu<B> {
    fn rip(&self) -> u64 {
        self.cpu.state.rip()
    }

    fn set_rip(&mut self, rip: u64) {
        self.cpu.state.set_rip(rip);
    }

    fn maybe_deliver_interrupt(&mut self) -> bool {
        if self.exit.is_some() {
            return false;
        }

        if self.cpu.pending.has_pending_event() {
            match self.cpu.deliver_pending_event(&mut self.bus) {
                Ok(()) => return true,
                Err(e) => {
                    self.exit = Some(e);
                    return true;
                }
            }
        }

        if !self.cpu.pending.external_interrupts.is_empty() {
            let before = self.cpu.pending.external_interrupts.len();
            match self.cpu.deliver_external_interrupt(&mut self.bus) {
                Ok(()) => {
                    if self.cpu.pending.external_interrupts.len() != before {
                        return true;
                    }
                }
                Err(e) => {
                    self.exit = Some(e);
                    return true;
                }
            }
        }

        false
    }

    fn on_retire_instructions(&mut self, instructions: u64, inhibit_interrupts: bool) {
        self.cpu.pending.retire_instructions(instructions);
        self.cpu.time.advance_cycles(instructions);
        let tsc = self.cpu.time.read_tsc();
        self.cpu.state.msr.tsc = tsc;
        if inhibit_interrupts {
            self.cpu.pending.inhibit_interrupts_for_one_instruction();
        }
    }
}

/// Minimal [`Interpreter`] implementation that executes Tier-0 (`interp::tier0`)
/// instructions.
///
/// This is intended for unit tests / integration glue. It resolves Tier-0 assist
/// exits so callers can drive the CPU using only [`Interpreter::exec_block`].
///
/// - Interrupt-related assists (`INT*`, `IRET*`, `CLI`, `STI`, `INTO`) are handled
///   via the architectural delivery logic in [`crate::interrupts`].
/// - Privileged/IO/time assists are handled via [`crate::assist::handle_assist`].
///
/// BIOS interrupt exits (real-mode `INT n` hypercalls) are surfaced by re-storing
/// the vector in [`crate::state::CpuState`] so the embedding can dispatch it.
#[derive(Debug, Default)]
pub struct Tier0Interpreter {
    /// Maximum Tier-0 instructions executed in one `exec_block` call.
    pub max_insts: u64,
    pub assist: AssistContext,
}

impl Tier0Interpreter {
    pub fn new(max_insts: u64) -> Self {
        Self {
            max_insts,
            assist: AssistContext::default(),
        }
    }
}

fn deliver_tier0_exception<B: crate::mem::CpuBus>(
    cpu: &mut Vcpu<B>,
    faulting_rip: u64,
    exception: crate::exception::Exception,
) {
    match exception_bridge::map_tier0_exception(&exception) {
        Ok(mapped) => {
            cpu.cpu.pending.raise_exception_fault(
                &mut cpu.cpu.state,
                mapped.exception,
                faulting_rip,
                mapped.error_code,
                mapped.cr2,
            );
            if let Err(exit) = cpu.cpu.deliver_pending_event(&mut cpu.bus) {
                cpu.exit = Some(exit);
            }
        }
        Err(exit) => {
            cpu.exit = Some(exit);
        }
    }
}

impl<B: crate::mem::CpuBus> Interpreter<Vcpu<B>> for Tier0Interpreter {
    fn exec_block(&mut self, cpu: &mut Vcpu<B>) -> InterpreterBlockExit {
        use aero_x86::{Mnemonic, OpKind, Register};

        use crate::exception::{AssistReason, Exception};
        use crate::interp::tier0::exec::StepExit;

        let max = self.max_insts.max(1);
        let cfg = crate::interp::tier0::Tier0Config::from_cpuid(&self.assist.features);
        let mut executed = 0u64;
        while executed < max {
            if cpu.exit.is_some() {
                break;
            }

            // Interrupts are delivered at instruction boundaries.
            if cpu.maybe_deliver_interrupt() {
                continue;
            }
            if cpu.cpu.state.halted {
                break;
            }

            let faulting_rip = cpu.cpu.state.rip();
            let step = match crate::interp::tier0::exec::step_with_config(
                &cfg,
                &mut cpu.cpu.state,
                &mut cpu.bus,
            ) {
                Ok(step) => step,
                Err(e) => {
                    deliver_tier0_exception(cpu, faulting_rip, e);
                    break;
                }
            };
            match step {
                StepExit::Continue => {
                    cpu.cpu.pending.retire_instruction();
                    cpu.cpu.time.advance_cycles(1);
                    cpu.cpu.state.msr.tsc = cpu.cpu.time.read_tsc();
                    executed += 1;
                    continue;
                }
                StepExit::ContinueInhibitInterrupts => {
                    cpu.cpu.pending.retire_instruction();
                    cpu.cpu.time.advance_cycles(1);
                    cpu.cpu.state.msr.tsc = cpu.cpu.time.read_tsc();
                    cpu.cpu.pending.inhibit_interrupts_for_one_instruction();
                    executed += 1;
                    continue;
                }
                StepExit::Branch => {
                    cpu.cpu.pending.retire_instruction();
                    cpu.cpu.time.advance_cycles(1);
                    cpu.cpu.state.msr.tsc = cpu.cpu.time.read_tsc();
                    executed += 1;
                    break;
                }
                StepExit::Halted => {
                    cpu.cpu.pending.retire_instruction();
                    cpu.cpu.time.advance_cycles(1);
                    cpu.cpu.state.msr.tsc = cpu.cpu.time.read_tsc();
                    executed += 1;
                    break;
                }
                StepExit::BiosInterrupt(vector) => {
                    // Tier-0 surfaces BIOS ROM stubs (`HLT; IRET`) as a `BiosInterrupt` exit when
                    // real/v8086-mode vector delivery transfers control into the stub.
                    //
                    // `step()` consumes the recorded vector via
                    // `take_pending_bios_int()`, but this `Interpreter` trait does not return it.
                    // Re-store the vector in CPU state so the embedding can observe and dispatch
                    // it before resuming execution at the stub's `IRET`.
                    cpu.cpu.state.set_pending_bios_int(vector);
                    cpu.cpu.pending.retire_instruction();
                    cpu.cpu.time.advance_cycles(1);
                    cpu.cpu.state.msr.tsc = cpu.cpu.time.read_tsc();
                    executed += 1;
                    break;
                }
                StepExit::Assist(AssistReason::Interrupt) => {
                    // Decode the instruction again to execute the interrupt/flag semantics.
                    let ip = cpu.cpu.state.rip();
                    let fetch_addr = cpu
                        .cpu
                        .state
                        .apply_a20(cpu.cpu.state.seg_base_reg(Register::CS).wrapping_add(ip));
                    let bytes = match fetch_wrapped(&cpu.cpu.state, &mut cpu.bus, fetch_addr, 15) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            cpu.cpu.state.apply_exception_side_effects(&e);
                            deliver_tier0_exception(cpu, ip, e);
                            break;
                        }
                    };
                    let bitness = cpu.cpu.state.bitness();
                    let addr_size_override = has_addr_size_override(&bytes, bitness);
                    let decoded = match aero_x86::decode(&bytes, ip, bitness) {
                        Ok(decoded) => decoded,
                        Err(_) => {
                            deliver_tier0_exception(cpu, ip, Exception::InvalidOpcode);
                            break;
                        }
                    };

                    let outcome = match crate::interrupts::exec_interrupt_assist_decoded(
                        &mut cpu.cpu,
                        &mut cpu.bus,
                        &decoded,
                        addr_size_override,
                    ) {
                        Ok(outcome) => outcome,
                        Err(exit) => {
                            cpu.exit = Some(exit);
                            break;
                        }
                    };
                    match outcome {
                        crate::interrupts::InterruptAssistOutcome::Retired {
                            inhibit_interrupts,
                            ..
                        } => {
                            cpu.cpu.pending.retire_instruction();
                            cpu.cpu.time.advance_cycles(1);
                            cpu.cpu.state.msr.tsc = cpu.cpu.time.read_tsc();
                            if inhibit_interrupts {
                                cpu.cpu.pending.inhibit_interrupts_for_one_instruction();
                            }
                            executed += 1;
                        }
                        crate::interrupts::InterruptAssistOutcome::FaultDelivered => {}
                    }

                    // Preserve basic-block behavior: treat this instruction as a block boundary.
                    break;
                }
                StepExit::Assist(_reason) => {
                    // Some privileged assists (notably `MOV SS, r/m16` and `POP SS`) create an
                    // interrupt shadow, inhibiting maskable interrupts for the following
                    // instruction. Decode here so we can update the interrupt bookkeeping in
                    // `PendingEventState`.
                    let ip = cpu.cpu.state.rip();
                    let fetch_addr = cpu
                        .cpu
                        .state
                        .apply_a20(cpu.cpu.state.seg_base_reg(Register::CS).wrapping_add(ip));
                    let bytes = match fetch_wrapped(&cpu.cpu.state, &mut cpu.bus, fetch_addr, 15) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            cpu.cpu.state.apply_exception_side_effects(&e);
                            deliver_tier0_exception(cpu, ip, e);
                            break;
                        }
                    };
                    let bitness = cpu.cpu.state.bitness();
                    // Keep address-size override prefix state in sync with `assist::handle_assist`.
                    let addr_size_override = has_addr_size_override(&bytes, bitness);
                    let decoded = match aero_x86::decode(&bytes, ip, bitness) {
                        Ok(decoded) => decoded,
                        Err(_) => {
                            deliver_tier0_exception(cpu, ip, Exception::InvalidOpcode);
                            break;
                        }
                    };
                    let inhibits_interrupt =
                        matches!(decoded.instr.mnemonic(), Mnemonic::Mov | Mnemonic::Pop)
                            && decoded.instr.op_count() > 0
                            && decoded.instr.op_kind(0) == OpKind::Register
                            && decoded.instr.op0_register() == Register::SS;

                    // `handle_assist_decoded` does not implicitly sync paging state (unlike
                    // `handle_assist`), so keep the bus coherent before and after.
                    cpu.bus.sync(&cpu.cpu.state);
                    let res = handle_assist_decoded(
                        &mut self.assist,
                        &mut cpu.cpu.time,
                        &mut cpu.cpu.state,
                        &mut cpu.bus,
                        &decoded,
                        addr_size_override,
                    )
                    .map_err(|e| (ip, e));
                    cpu.bus.sync(&cpu.cpu.state);
                    match res {
                        Ok(()) => {
                            cpu.cpu.pending.retire_instruction();
                            cpu.cpu.time.advance_cycles(1);
                            cpu.cpu.state.msr.tsc = cpu.cpu.time.read_tsc();
                            if inhibits_interrupt {
                                cpu.cpu.pending.inhibit_interrupts_for_one_instruction();
                            }
                            executed += 1;
                        }
                        Err((faulting_rip, e)) => {
                            deliver_tier0_exception(cpu, faulting_rip, e);
                        }
                    }
                    break;
                }
            }
        }

        InterpreterBlockExit {
            next_rip: cpu.cpu.state.rip(),
            instructions_retired: executed,
        }
    }
}
