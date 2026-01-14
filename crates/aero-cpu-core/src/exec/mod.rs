use crate::assist::{handle_assist_decoded, has_addr_size_override, AssistContext};
use crate::jit::runtime::{CompileRequestSink, JitBackend, JitBlockExit, JitRuntime};
use aero_perf::PerfWorker;

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

impl aero_perf::InstructionRetirement for StepOutcome {
    #[inline(always)]
    fn instructions_retired(&self) -> u64 {
        match *self {
            StepOutcome::InterruptDelivered => 0,
            StepOutcome::Block {
                instructions_retired,
                ..
            } => instructions_retired,
        }
    }
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

    /// Execute a single tiered step while updating a [`PerfWorker`].
    ///
    /// This is a convenience wrapper around [`Self::step`] that translates the
    /// dispatcher's retirement semantics into [`aero_perf`] counters:
    ///
    /// - [`StepOutcome::Block`] retires `instructions_retired` guest architectural
    ///   instructions.
    /// - JIT rollback exits report `instructions_retired == 0`; those must **not**
    ///   advance instruction counters.
    /// - [`StepOutcome::InterruptDelivered`] does not retire an instruction and
    ///   must **not** advance instruction counters.
    ///
    /// # REP iteration counting (optional)
    ///
    /// `REP*` string instructions retire as *one* architectural instruction even
    /// though they may iterate many times. Embeddings that want to track those
    /// iterations should use [`Tier0RepIterTracker`] around Tier-0 interpreter
    /// *single-instruction* steps:
    ///
    /// 1. Before executing the decoded instruction, call
    ///    [`Tier0RepIterTracker::begin`] (or [`Tier0RepIterTracker::begin_from_bytes`]
    ///    if you already have the fetched instruction bytes).
    /// 2. After the instruction executes, call [`Tier0RepIterTracker::finish`]
    ///    to record `rep_iterations`.
    ///
    /// The tracker reads the `CX/ECX/RCX` count register before/after execution
    /// **only** when the decoded instruction has a `REP`/`REPNE` prefix and is a
    /// string mnemonic, keeping overhead out of the common non-string case.
    pub fn step_with_perf(&mut self, cpu: &mut B::Cpu, perf: &mut PerfWorker) -> StepOutcome {
        let outcome = self.step(cpu);
        aero_perf::retire_from_step_outcome(perf, &outcome);
        outcome
    }

    pub fn run_blocks(&mut self, cpu: &mut B::Cpu, mut blocks: u64) {
        while blocks > 0 {
            match self.step(cpu) {
                StepOutcome::InterruptDelivered => continue,
                StepOutcome::Block { .. } => blocks -= 1,
            }
        }
    }

    /// Run `blocks` tiered-execution blocks while updating a [`PerfWorker`].
    ///
    /// This is the `PerfWorker`-aware sibling of [`Self::run_blocks`]. It uses
    /// [`Self::step_with_perf`] for retirement semantics, while preserving the
    /// `run_blocks` behavior of not charging `InterruptDelivered` toward the
    /// block budget.
    pub fn run_blocks_with_perf(
        &mut self,
        cpu: &mut B::Cpu,
        perf: &mut PerfWorker,
        mut blocks: u64,
    ) {
        while blocks > 0 {
            match self.step_with_perf(cpu, perf) {
                StepOutcome::InterruptDelivered => continue,
                StepOutcome::Block { .. } => blocks -= 1,
            }
        }
    }
}

/// Helper for tracking Tier-0 `REP*` string instruction iterations.
///
/// `REP*` string instructions retire as *one* architectural instruction but may
/// iterate many times internally. Tier-0 executes those iterations inside a
/// single interpreter step, so the most reliable way to derive the iteration
/// count is to observe the architectural count register (`CX`/`ECX`/`RCX`)
/// before/after executing the instruction.
///
/// This helper intentionally does **not** fetch/decode instructions on its own.
/// Callers should pass the already-decoded instruction and the already-parsed
/// address-size override prefix state from their Tier-0 step loop.
#[derive(Debug, Clone, Copy)]
pub struct Tier0RepIterTracker {
    count_reg: aero_x86::Register,
    count_mask: u64,
    count_before: u64,
}

impl Tier0RepIterTracker {
    /// Begin tracking `REP*` iterations for a single Tier-0 interpreter step.
    ///
    /// Returns `None` for non-`REP*` or non-string instructions (fast path).
    #[inline]
    pub fn begin(
        state: &crate::state::CpuState,
        decoded: &aero_x86::DecodedInst,
        addr_size_override: bool,
    ) -> Option<Self> {
        let instr = &decoded.instr;
        let is_rep = instr.has_rep_prefix() || instr.has_repne_prefix();
        if !is_rep {
            return None;
        }
        if !is_string_mnemonic(instr.mnemonic()) {
            return None;
        }

        let addr_bits = effective_addr_size(state.bitness(), addr_size_override);
        let count_reg = string_count_reg(addr_bits);
        let count_mask = crate::state::mask_bits(addr_bits);
        let count_before = state.read_reg(count_reg) & count_mask;

        Some(Self {
            count_reg,
            count_mask,
            count_before,
        })
    }

    /// Begin tracking `REP*` iterations for a single Tier-0 interpreter step,
    /// deriving the address-size override prefix state from the fetched
    /// instruction bytes.
    ///
    /// This is a convenience wrapper for Tier-0 step loops that already fetched
    /// up to 15 bytes (e.g. `CpuBus::fetch` / `fetch_wrapped`) and want to avoid
    /// re-implementing prefix scanning just to determine whether `67` was
    /// present.
    ///
    /// The prefix scan runs **only** for decoded instructions that have a
    /// `REP`/`REPNE` prefix and are string mnemonics, so it does not add overhead
    /// to the common non-string case.
    #[inline]
    pub fn begin_from_bytes(
        state: &crate::state::CpuState,
        decoded: &aero_x86::DecodedInst,
        bytes: &[u8; 15],
    ) -> Option<Self> {
        let instr = &decoded.instr;
        let is_rep = instr.has_rep_prefix() || instr.has_repne_prefix();
        if !is_rep {
            return None;
        }
        if !is_string_mnemonic(instr.mnemonic()) {
            return None;
        }

        let addr_size_override = has_addr_size_override(bytes, state.bitness());
        let addr_bits = effective_addr_size(state.bitness(), addr_size_override);
        let count_reg = string_count_reg(addr_bits);
        let count_mask = crate::state::mask_bits(addr_bits);
        let count_before = state.read_reg(count_reg) & count_mask;

        Some(Self {
            count_reg,
            count_mask,
            count_before,
        })
    }

    /// Finish tracking and record the number of iterations into `perf`.
    #[inline]
    pub fn finish(self, state: &crate::state::CpuState, perf: &mut PerfWorker) {
        let iterations = self.iterations(state);
        if iterations != 0 {
            perf.add_rep_iterations(iterations);
        }
    }

    /// Finish tracking and return the iteration count without updating any counters.
    #[inline]
    pub fn iterations(self, state: &crate::state::CpuState) -> u64 {
        let count_after = state.read_reg(self.count_reg) & self.count_mask;
        self.count_before.wrapping_sub(count_after) & self.count_mask
    }
}

#[inline]
fn is_string_mnemonic(m: aero_x86::Mnemonic) -> bool {
    use aero_x86::Mnemonic;
    matches!(
        m,
        Mnemonic::Movsb
            | Mnemonic::Movsw
            | Mnemonic::Movsd
            | Mnemonic::Movsq
            | Mnemonic::Stosb
            | Mnemonic::Stosw
            | Mnemonic::Stosd
            | Mnemonic::Stosq
            | Mnemonic::Lodsb
            | Mnemonic::Lodsw
            | Mnemonic::Lodsd
            | Mnemonic::Lodsq
            | Mnemonic::Cmpsb
            | Mnemonic::Cmpsw
            | Mnemonic::Cmpsd
            | Mnemonic::Cmpsq
            | Mnemonic::Scasb
            | Mnemonic::Scasw
            | Mnemonic::Scasd
            | Mnemonic::Scasq
            | Mnemonic::Insb
            | Mnemonic::Insw
            | Mnemonic::Insd
            | Mnemonic::Outsb
            | Mnemonic::Outsw
            | Mnemonic::Outsd
    )
}

#[inline]
fn effective_addr_size(bitness: u32, addr_size_override: bool) -> u32 {
    match bitness {
        16 => {
            if addr_size_override {
                32
            } else {
                16
            }
        }
        32 => {
            if addr_size_override {
                16
            } else {
                32
            }
        }
        64 => {
            if addr_size_override {
                32
            } else {
                64
            }
        }
        _ => bitness,
    }
}

#[inline]
fn string_count_reg(addr_bits: u32) -> aero_x86::Register {
    use aero_x86::Register;
    match addr_bits {
        16 => Register::CX,
        32 => Register::ECX,
        _ => Register::RCX,
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

        if !self.cpu.pending.external_interrupts().is_empty() {
            let before = self.cpu.pending.external_interrupts().len();
            match self.cpu.deliver_external_interrupt(&mut self.bus) {
                Ok(()) => {
                    if self.cpu.pending.external_interrupts().len() != before {
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
/// - Privileged/IO/time assists are handled via [`crate::assist::handle_assist_decoded`].
///
/// BIOS interrupt exits (real-mode `INT n` hypercalls) are surfaced by re-storing
/// the vector in [`crate::state::CpuState`] so the embedding can dispatch it.
#[derive(Debug, Default)]
pub struct Tier0Interpreter {
    /// Maximum Tier-0 instructions executed in one `exec_block` call.
    pub max_insts: u64,
    pub assist: AssistContext,
    decode_cache: Tier0DecodeCache,
}

impl Tier0Interpreter {
    pub fn new(max_insts: u64) -> Self {
        Self {
            max_insts,
            assist: AssistContext::default(),
            decode_cache: Tier0DecodeCache::default(),
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn decode_cache_stats(&self) -> Tier0DecodeCacheStats {
        self.decode_cache.stats()
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

        use crate::exception::AssistReason;
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
            let step = match crate::interp::tier0::exec::step_with_config_and_decoder(
                &cfg,
                &mut cpu.cpu.state,
                &mut cpu.bus,
                |bytes, ip, bitness| self.decode_cache.decode(bytes, ip, bitness),
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
                StepExit::Assist {
                    reason: AssistReason::Interrupt,
                    decoded,
                    addr_size_override,
                } => {
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
                StepExit::Assist {
                    reason: _reason,
                    decoded,
                    addr_size_override,
                } => {
                    // Some privileged assists (notably `MOV SS, r/m16` and `POP SS`) create an
                    // interrupt shadow, inhibiting maskable interrupts for the following
                    // instruction. Use the already decoded instruction so we can update the
                    // interrupt bookkeeping in
                    // `PendingEventState`.
                    let inhibits_interrupt =
                        matches!(decoded.instr.mnemonic(), Mnemonic::Mov | Mnemonic::Pop)
                            && decoded.instr.op_count() > 0
                            && decoded.instr.op_kind(0) == OpKind::Register
                            && decoded.instr.op0_register() == Register::SS;

                    // `handle_assist_decoded` does not implicitly sync paging state (unlike
                    // `handle_assist`), so keep the bus coherent before and after.
                    let ip = cpu.cpu.state.rip();
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

// ---- Tier-0 decode cache ----------------------------------------------------

const TIER0_DECODE_CACHE_SIZE: usize = 256;

#[derive(Debug, Clone)]
struct Tier0DecodeCacheEntry {
    bitness: u32,
    rip: u64,
    bytes: [u8; 15],
    decoded: aero_x86::DecodedInst,
}

#[derive(Debug)]
struct Tier0DecodeCache {
    entries: [Option<Tier0DecodeCacheEntry>; TIER0_DECODE_CACHE_SIZE],
    #[cfg(any(test, debug_assertions))]
    hits: u64,
    #[cfg(any(test, debug_assertions))]
    misses: u64,
}

impl Default for Tier0DecodeCache {
    fn default() -> Self {
        Self {
            entries: core::array::from_fn(|_| None),
            #[cfg(any(test, debug_assertions))]
            hits: 0,
            #[cfg(any(test, debug_assertions))]
            misses: 0,
        }
    }
}

impl Tier0DecodeCache {
    #[inline]
    fn index(rip: u64, bitness: u32) -> usize {
        debug_assert!(TIER0_DECODE_CACHE_SIZE.is_power_of_two());

        // Direct-mapped cache: hash `rip` + `bitness` into a small fixed-size
        // table. This avoids the O(N) linear probe of a fully associative cache
        // while still covering typical hot loops.
        let mut x = rip.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (bitness as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        x ^= x >> 33;
        x as usize & (TIER0_DECODE_CACHE_SIZE - 1)
    }

    #[inline]
    fn decode(
        &mut self,
        bytes: &[u8; 15],
        rip: u64,
        bitness: u32,
    ) -> Result<aero_x86::DecodedInst, aero_x86::DecodeError> {
        let idx = Self::index(rip, bitness);
        if let Some(hit) = &self.entries[idx] {
            let len = hit.decoded.len as usize;
            if hit.bitness == bitness
                && hit.rip == rip
                && len > 0
                && len <= 15
                // Self-modifying code safety: verify the instruction bytes still match.
                && hit.bytes[..len] == bytes[..len]
            {
                #[cfg(any(test, debug_assertions))]
                {
                    self.hits += 1;
                }
                return Ok(hit.decoded.clone());
            }
        }

        #[cfg(any(test, debug_assertions))]
        {
            self.misses += 1;
        }

        let decoded = aero_x86::decode(bytes, rip, bitness)?;
        self.entries[idx] = Some(Tier0DecodeCacheEntry {
            bitness,
            rip,
            bytes: *bytes,
            decoded: decoded.clone(),
        });
        Ok(decoded)
    }

    #[cfg(any(test, debug_assertions))]
    fn stats(&self) -> Tier0DecodeCacheStats {
        Tier0DecodeCacheStats {
            hits: self.hits,
            misses: self.misses,
        }
    }
}

#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tier0DecodeCacheStats {
    pub hits: u64,
    pub misses: u64,
}
