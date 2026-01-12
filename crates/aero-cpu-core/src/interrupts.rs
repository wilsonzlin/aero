//! Interrupt/exception delivery (IVT/IDT), privilege transitions, and IRET for
//! the Tier-0 CPU model (`state::CpuState` + `mem::CpuBus`).
//!
//! The `state::CpuState` structure is part of the JIT ABI and intentionally
//! contains only architecturally visible state. Any additional bookkeeping
//! needed by the interpreter (pending events, interrupt shadows, exception
//! nesting, external interrupt FIFO, etc) lives in [`PendingEventState`].

use std::collections::VecDeque;

use aero_x86::{DecodedInst, Mnemonic, Register};

use crate::exception::Exception as CpuException;
use crate::exceptions::{Exception, InterruptSource, PendingEvent};
use crate::linear_mem::{
    read_u16_wrapped, read_u32_wrapped, read_u64_wrapped, write_u16_wrapped, write_u32_wrapped,
    write_u64_wrapped,
};
use crate::mem::CpuBus;
use crate::state::{
    self, gpr, CpuMode, RFLAGS_IF, RFLAGS_IOPL_MASK, RFLAGS_OF, RFLAGS_RESERVED1, RFLAGS_TF,
    RFLAGS_VIF, RFLAGS_VIP, RFLAGS_VM,
};
use crate::time::TimeSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuExit {
    /// Failure to deliver an exception (including #DF) that results in a reset.
    TripleFault,
    /// Non-architectural memory/bus fault (e.g. unmapped physical memory / MMIO failure).
    MemoryFault,
    /// The interpreter decoded an instruction but has no implementation for it.
    UnimplementedInstruction(&'static str),
}

/// External interrupt controller interface.
pub trait InterruptController {
    /// Returns the next pending external interrupt vector, if any.
    fn poll_interrupt(&mut self) -> Option<u8>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateType {
    Interrupt,
    Trap,
    Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExceptionClass {
    Benign,
    Contributory,
    PageFault,
    DoubleFault,
}

impl ExceptionClass {
    fn of(exception: Exception) -> Self {
        match exception {
            Exception::PageFault => Self::PageFault,
            Exception::DoubleFault => Self::DoubleFault,
            Exception::InvalidTss
            | Exception::SegmentNotPresent
            | Exception::StackFault
            | Exception::GeneralProtection => Self::Contributory,
            _ => Self::Benign,
        }
    }
}

fn should_double_fault(first: Exception, second: Exception) -> bool {
    use ExceptionClass as C;
    matches!(
        (C::of(first), C::of(second)),
        (C::Contributory, C::Contributory | C::PageFault)
            | (C::PageFault, C::Contributory | C::PageFault)
    )
}

#[derive(Debug, Clone, Copy)]
struct VectorDelivery {
    vector: u8,
    saved_rip: u64,
    error_code: Option<u32>,
    is_interrupt: bool,
    source: InterruptSource,
}

fn deliver_cpu_exception<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    exception: CpuException,
    saved_rip: u64,
) -> Result<(), CpuExit> {
    // Keep architecturally visible side effects (CR2) in sync with the actual fault.
    state.apply_exception_side_effects(&exception);

    match exception {
        CpuException::PageFault { error_code, .. } => deliver_exception(
            bus,
            state,
            pending,
            Exception::PageFault,
            saved_rip,
            Some(error_code),
        ),
        CpuException::GeneralProtection(code) => deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
            Some(code as u32),
        ),
        CpuException::SegmentNotPresent(code) => deliver_exception(
            bus,
            state,
            pending,
            Exception::SegmentNotPresent,
            saved_rip,
            Some(code as u32),
        ),
        CpuException::StackSegment(code) => deliver_exception(
            bus,
            state,
            pending,
            Exception::StackFault,
            saved_rip,
            Some(code as u32),
        ),
        CpuException::InvalidTss(code) => deliver_exception(
            bus,
            state,
            pending,
            Exception::InvalidTss,
            saved_rip,
            Some(code as u32),
        ),
        CpuException::DivideError => {
            deliver_exception(bus, state, pending, Exception::DivideError, saved_rip, None)
        }
        CpuException::InvalidOpcode | CpuException::Unimplemented(_) => deliver_exception(
            bus,
            state,
            pending,
            Exception::InvalidOpcode,
            saved_rip,
            None,
        ),
        CpuException::DeviceNotAvailable => deliver_exception(
            bus,
            state,
            pending,
            Exception::DeviceNotAvailable,
            saved_rip,
            None,
        ),
        CpuException::X87Fpu => {
            deliver_exception(bus, state, pending, Exception::X87Fpu, saved_rip, None)
        }
        CpuException::SimdFloatingPointException => deliver_exception(
            bus,
            state,
            pending,
            Exception::SimdFloatingPoint,
            saved_rip,
            None,
        ),
        CpuException::MemoryFault => Err(CpuExit::MemoryFault),
    }
}

/// Execute a paging-protected memory access that should be treated as a
/// supervisor ("system") access regardless of the current CPL.
///
/// On real hardware, reads of system structures like the IDT and TSS are not
/// subject to user/supervisor page restrictions even when the interrupted code
/// was running at CPL3. Our paging bus caches CPL, so emulate this by
/// temporarily forcing CS.RPL=0 for the duration of the access.
fn with_supervisor_access<B: CpuBus, R>(
    bus: &mut B,
    state: &mut state::CpuState,
    f: impl FnOnce(&mut B, &state::CpuState) -> R,
) -> R {
    if state.cpl() != 3 {
        return f(bus, state);
    }

    let old_cs = state.segments.cs.selector;
    state.segments.cs.selector &= !0b11;
    bus.sync(state);
    let res = f(bus, state);
    state.segments.cs.selector = old_cs;
    bus.sync(state);
    res
}

#[derive(Debug, Clone, Copy)]
struct IdtGate32 {
    offset: u32,
    selector: u16,
    gate_type: GateType,
    dpl: u8,
    present: bool,
}

#[derive(Debug, Clone, Copy)]
struct IdtGate64 {
    offset: u64,
    selector: u16,
    gate_type: GateType,
    dpl: u8,
    present: bool,
    ist: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptFrame {
    Real16,
    Protected32 { stack_switched: bool },
    Long64 { stack_switched: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushOutcome {
    Pushed,
    NestedExceptionDelivered,
}

/// Extra CPU-core state that is intentionally *not* part of the JIT ABI.
#[derive(Debug, Default)]
pub struct PendingEventState {
    pending_event: Option<PendingEvent>,
    /// FIFO of externally injected interrupts (PIC/APIC).
    pub external_interrupts: VecDeque<u8>,

    /// Interrupt shadow counter (STI / MOV SS / POP SS).
    interrupt_inhibit: u8,

    // --- Exception nesting / double fault escalation ---
    delivering_exception: Option<Exception>,
    exception_depth: u32,

    // --- IRET bookkeeping ---
    interrupt_frames: Vec<InterruptFrame>,
}

impl PendingEventState {
    /// Queue a faulting exception for delivery at the next instruction boundary.
    ///
    /// For page faults this will also update CR2 in [`state::CpuState`].
    pub fn raise_exception_fault(
        &mut self,
        state: &mut state::CpuState,
        exception: Exception,
        faulting_rip: u64,
        error_code: Option<u32>,
        cr2: Option<u64>,
    ) {
        if exception == Exception::PageFault {
            if let Some(addr) = cr2 {
                state.control.cr2 = addr;
            }
        }
        self.pending_event = Some(PendingEvent::Fault {
            exception,
            saved_rip: faulting_rip,
            error_code,
        });
    }

    /// Queue a software interrupt (e.g. `INT n`) for delivery at the next
    /// instruction boundary.
    pub fn raise_software_interrupt(&mut self, vector: u8, return_rip: u64) {
        self.pending_event = Some(PendingEvent::Interrupt {
            vector,
            saved_rip: return_rip,
            source: InterruptSource::Software,
        });
    }

    /// Inject an external interrupt vector (e.g. from PIC/APIC).
    pub fn inject_external_interrupt(&mut self, vector: u8) {
        self.external_interrupts.push_back(vector);
    }

    /// Inhibit maskable interrupts for exactly one instruction.
    ///
    /// This models the interrupt shadow after `STI` as well as `MOV SS`/`POP SS`
    /// semantics. The execution engine should call [`Self::retire_instruction`]
    /// after each successfully executed instruction to age this counter.
    pub fn inhibit_interrupts_for_one_instruction(&mut self) {
        self.interrupt_inhibit = 1;
    }

    /// Return the current interrupt-inhibit ("interrupt shadow") counter.
    ///
    /// The Tier-0 model currently only uses values `0` and `1`:
    /// - `0`: interrupts are not inhibited by STI/MOV SS/POP SS shadowing.
    /// - `1`: inhibit maskable interrupts for the next instruction.
    pub fn interrupt_inhibit(&self) -> u8 {
        self.interrupt_inhibit
    }

    /// Restore the interrupt-inhibit ("interrupt shadow") counter.
    ///
    /// The current implementation only supports `0` and `1`; any non-zero value
    /// is clamped to `1` to keep invariants stable across snapshot restores.
    pub fn set_interrupt_inhibit(&mut self, v: u8) {
        self.interrupt_inhibit = (v != 0) as u8;
    }

    /// Call after each successfully executed instruction to update the interrupt
    /// shadow state.
    pub fn retire_instruction(&mut self) {
        if self.interrupt_inhibit > 0 {
            self.interrupt_inhibit -= 1;
        }
    }

    /// Bulk version of [`Self::retire_instruction`].
    ///
    /// This must match the semantics of calling [`Self::retire_instruction`] `instructions` times.
    pub fn retire_instructions(&mut self, instructions: u64) {
        if self.interrupt_inhibit == 0 || instructions == 0 {
            return;
        }
        let dec = instructions.min(self.interrupt_inhibit as u64) as u8;
        self.interrupt_inhibit -= dec;
    }

    /// Whether there is a pending exception/interrupt waiting to be delivered.
    ///
    /// This is primarily used by execution glue (`exec::Vcpu`) to decide whether
    /// calling [`deliver_pending_event`] will actually deliver anything.
    pub fn has_pending_event(&self) -> bool {
        self.pending_event.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::PendingEventState;

    #[test]
    fn interrupt_inhibit_defaults_to_zero() {
        let pending = PendingEventState::default();
        assert_eq!(pending.interrupt_inhibit(), 0);
    }

    #[test]
    fn inhibit_interrupts_for_one_instruction_sets_shadow_to_one() {
        let mut pending = PendingEventState::default();
        pending.inhibit_interrupts_for_one_instruction();
        assert_eq!(pending.interrupt_inhibit(), 1);
    }

    #[test]
    fn retire_instruction_decrements_interrupt_shadow_to_zero() {
        let mut pending = PendingEventState::default();
        pending.inhibit_interrupts_for_one_instruction();
        pending.retire_instruction();
        assert_eq!(pending.interrupt_inhibit(), 0);
    }

    #[test]
    fn set_interrupt_inhibit_restores_and_clamps() {
        let mut pending = PendingEventState::default();

        pending.set_interrupt_inhibit(1);
        assert_eq!(pending.interrupt_inhibit(), 1);

        pending.set_interrupt_inhibit(0);
        assert_eq!(pending.interrupt_inhibit(), 0);

        // Any non-zero value is clamped to 1.
        pending.set_interrupt_inhibit(2);
        assert_eq!(pending.interrupt_inhibit(), 1);
        pending.set_interrupt_inhibit(u8::MAX);
        assert_eq!(pending.interrupt_inhibit(), 1);
    }
}

/// Convenience wrapper that owns both the JIT ABI state and the non-ABI
/// interrupt bookkeeping.
#[derive(Debug, Default)]
pub struct CpuCore {
    pub state: state::CpuState,
    pub pending: PendingEventState,
    pub time: TimeSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptAssistOutcome {
    /// The instruction retired successfully.
    ///
    /// `block_boundary` is a conservative hint for Tier-0 batch execution: the
    /// instruction either transferred control (`INT*`, `IRET*`, taken `INTO`) or
    /// otherwise should terminate the current basic block.
    Retired {
        block_boundary: bool,
        /// Whether maskable interrupts should be inhibited for exactly one
        /// subsequent instruction (STI interrupt shadow).
        inhibit_interrupts: bool,
    },
    /// The instruction faulted and an exception was delivered; the instruction
    /// did not retire.
    FaultDelivered,
}

impl CpuCore {
    pub fn new(mode: CpuMode) -> Self {
        let state = state::CpuState::new(mode);
        let mut time = TimeSource::default();
        time.set_tsc(state.msr.tsc);
        Self {
            state,
            pending: PendingEventState::default(),
            time,
        }
    }

    pub fn deliver_pending_event<B: CpuBus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        deliver_pending_event(&mut self.state, bus, &mut self.pending)
    }

    pub fn deliver_external_interrupt<B: CpuBus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        deliver_external_interrupt(&mut self.state, bus, &mut self.pending)
    }

    pub fn poll_and_deliver_external_interrupt<B: CpuBus, C: InterruptController>(
        &mut self,
        bus: &mut B,
        ctrl: &mut C,
    ) -> Result<(), CpuExit> {
        poll_and_deliver_external_interrupt(&mut self.state, bus, &mut self.pending, ctrl)
    }

    pub fn iret<B: CpuBus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        iret(&mut self.state, bus, &mut self.pending)
    }
}

impl core::ops::Deref for CpuCore {
    type Target = state::CpuState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl core::ops::DerefMut for CpuCore {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

/// Execute an interrupt-related instruction that Tier-0 treats as an "assist".
///
/// This helper is the canonical implementation of the architectural semantics
/// for `CLI`/`STI`/`INT*`/`INTO`/`IRET*` because it has access to
/// [`PendingEventState`] (interrupt shadow + IRET frame bookkeeping).
pub fn exec_interrupt_assist_decoded<B: CpuBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    decoded: &DecodedInst,
    _addr_size_override: bool,
) -> Result<InterruptAssistOutcome, CpuExit> {
    let ip = cpu.state.rip();
    let next_ip = ip.wrapping_add(decoded.len as u64) & cpu.state.mode.ip_mask();

    match decoded.instr.mnemonic() {
        Mnemonic::Cli => {
            let cpl = cpu.state.cpl();
            let iopl = ((cpu.state.rflags() & RFLAGS_IOPL_MASK) >> 12) as u8;
            if !matches!(cpu.state.mode, CpuMode::Real | CpuMode::Vm86) && cpl > iopl {
                cpu.pending.raise_exception_fault(
                    &mut cpu.state,
                    Exception::GeneralProtection,
                    ip,
                    Some(0),
                    None,
                );
                cpu.deliver_pending_event(bus)?;
                return Ok(InterruptAssistOutcome::FaultDelivered);
            }
            cpu.state.set_flag(RFLAGS_IF, false);
            cpu.state.set_rip(next_ip);
            Ok(InterruptAssistOutcome::Retired {
                block_boundary: false,
                inhibit_interrupts: false,
            })
        }
        Mnemonic::Sti => {
            let cpl = cpu.state.cpl();
            let iopl = ((cpu.state.rflags() & RFLAGS_IOPL_MASK) >> 12) as u8;
            if !matches!(cpu.state.mode, CpuMode::Real | CpuMode::Vm86) && cpl > iopl {
                cpu.pending.raise_exception_fault(
                    &mut cpu.state,
                    Exception::GeneralProtection,
                    ip,
                    Some(0),
                    None,
                );
                cpu.deliver_pending_event(bus)?;
                return Ok(InterruptAssistOutcome::FaultDelivered);
            }
            cpu.state.set_flag(RFLAGS_IF, true);
            cpu.state.set_rip(next_ip);
            Ok(InterruptAssistOutcome::Retired {
                block_boundary: false,
                inhibit_interrupts: true,
            })
        }
        Mnemonic::Int | Mnemonic::Int1 | Mnemonic::Int3 | Mnemonic::Into => {
            let vector = match decoded.instr.mnemonic() {
                Mnemonic::Int => decoded.instr.immediate8(),
                Mnemonic::Int1 => 1,
                Mnemonic::Int3 => 3,
                Mnemonic::Into => {
                    if !cpu.state.get_flag(RFLAGS_OF) {
                        cpu.state.set_rip(next_ip);
                        return Ok(InterruptAssistOutcome::Retired {
                            block_boundary: false,
                            inhibit_interrupts: false,
                        });
                    }
                    4
                }
                _ => unreachable!(),
            };

            if matches!(cpu.state.mode, CpuMode::Real | CpuMode::Vm86) {
                cpu.state.set_pending_bios_int(vector);
            }

            cpu.pending.raise_software_interrupt(vector, next_ip);
            cpu.deliver_pending_event(bus)?;
            Ok(InterruptAssistOutcome::Retired {
                block_boundary: true,
                inhibit_interrupts: false,
            })
        }
        Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq => {
            cpu.iret(bus)?;
            if matches!(cpu.state.mode, CpuMode::Real | CpuMode::Vm86) {
                cpu.state.clear_pending_bios_int();
            }
            Ok(InterruptAssistOutcome::Retired {
                block_boundary: true,
                inhibit_interrupts: false,
            })
        }
        other => panic!("unsupported interrupt assist mnemonic: {other:?}"),
    }
}

/// Deliver any pending event (exception, software interrupt, etc).
pub fn deliver_pending_event<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
) -> Result<(), CpuExit> {
    let Some(event) = pending.pending_event.take() else {
        return Ok(());
    };
    bus.sync(state);
    deliver_event(state, bus, pending, event)
}

/// Poll an interrupt controller and deliver an interrupt if permitted.
pub fn poll_and_deliver_external_interrupt<B: CpuBus, C: InterruptController>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
    ctrl: &mut C,
) -> Result<(), CpuExit> {
    if let Some(vector) = ctrl.poll_interrupt() {
        pending.inject_external_interrupt(vector);
    }
    deliver_external_interrupt(state, bus, pending)
}

/// Attempt to deliver the next queued external interrupt.
pub fn deliver_external_interrupt<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
) -> Result<(), CpuExit> {
    bus.sync(state);
    if pending.pending_event.is_some() {
        // Exceptions/traps/INTn take priority.
        return Ok(());
    }

    if (state.rflags() & RFLAGS_IF) == 0 {
        return Ok(());
    }

    if pending.interrupt_inhibit > 0 {
        return Ok(());
    }

    let Some(vector) = pending.external_interrupts.pop_front() else {
        return Ok(());
    };

    // Tier-0 treats `HLT` as a BIOS interrupt hypercall only when
    // `pending_bios_int_valid` is set. Software `INT n` assists already set this
    // in real/v8086 mode, but externally injected interrupts (PIC/APIC) can also
    // vector into BIOS ROM stubs that begin with `HLT; IRET`. Record the vector
    // here so the Tier-0 interpreter exits with `BiosInterrupt(vector)` instead
    // of permanently halting the VM.
    if matches!(state.mode, CpuMode::Real | CpuMode::Vm86) {
        state.set_pending_bios_int(vector);
    }

    // Maskable interrupts wake the CPU from `HLT` when they are actually delivered.
    state.halted = false;

    let saved_rip = state.rip();
    deliver_event(
        state,
        bus,
        pending,
        PendingEvent::Interrupt {
            vector,
            saved_rip,
            source: InterruptSource::External,
        },
    )
}

/// Execute an IRET/IRETD/IRETQ depending on the current mode.
pub fn iret<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
) -> Result<(), CpuExit> {
    bus.sync(state);
    let saved_rip = state.rip();
    let Some(frame) = pending.interrupt_frames.last().copied() else {
        // No pending frame; on real hardware this would be #GP(0).
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
            Some(0),
        );
    };

    let outcome = match frame {
        InterruptFrame::Real16 => iret_real(state, bus, pending, saved_rip)?,
        InterruptFrame::Protected32 { stack_switched } => {
            iret_protected(state, bus, pending, saved_rip, stack_switched)?
        }
        InterruptFrame::Long64 { stack_switched } => {
            iret_long(state, bus, pending, saved_rip, stack_switched)?
        }
    };

    if outcome == IretOutcome::Completed {
        pending.interrupt_frames.pop();
        bus.sync(state);
    }

    Ok(())
}

fn deliver_event<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
    event: PendingEvent,
) -> Result<(), CpuExit> {
    match event {
        PendingEvent::Fault {
            exception,
            saved_rip,
            error_code,
        } => deliver_exception(bus, state, pending, exception, saved_rip, error_code),
        PendingEvent::Trap { vector, saved_rip } => deliver_vector(
            bus,
            state,
            pending,
            VectorDelivery {
                vector,
                saved_rip,
                error_code: None,
                is_interrupt: false,
                source: InterruptSource::External,
            },
        ),
        PendingEvent::Interrupt {
            vector,
            saved_rip,
            source,
        } => deliver_vector(
            bus,
            state,
            pending,
            VectorDelivery {
                vector,
                saved_rip,
                error_code: None,
                is_interrupt: true,
                source,
            },
        ),
    }
}

fn deliver_exception<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    exception: Exception,
    saved_rip: u64,
    error_code: Option<u32>,
) -> Result<(), CpuExit> {
    if let Some(first) = pending.delivering_exception {
        if first == Exception::DoubleFault {
            return Err(CpuExit::TripleFault);
        }
        if exception != Exception::DoubleFault && should_double_fault(first, exception) {
            return deliver_exception(
                bus,
                state,
                pending,
                Exception::DoubleFault,
                saved_rip,
                Some(0),
            );
        }
    }

    let prev_delivering = pending.delivering_exception;
    pending.delivering_exception = Some(exception);
    pending.exception_depth = pending.exception_depth.saturating_add(1);

    let code = if exception.pushes_error_code() {
        Some(error_code.unwrap_or(0))
    } else {
        None
    };

    let res = deliver_vector(
        bus,
        state,
        pending,
        VectorDelivery {
            vector: exception.vector(),
            saved_rip,
            error_code: code,
            is_interrupt: false,
            source: InterruptSource::External,
        },
    );

    pending.exception_depth = pending.exception_depth.saturating_sub(1);
    pending.delivering_exception = prev_delivering;
    res
}

fn deliver_vector<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    delivery: VectorDelivery,
) -> Result<(), CpuExit> {
    match state.mode {
        CpuMode::Real | CpuMode::Vm86 => {
            deliver_real_mode(bus, state, pending, delivery.vector, delivery.saved_rip)
        }
        CpuMode::Protected => deliver_protected_mode(bus, state, pending, delivery),
        CpuMode::Long => deliver_long_mode(bus, state, pending, delivery),
    }
}

fn deliver_real_mode<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    vector: u8,
    saved_rip: u64,
) -> Result<(), CpuExit> {
    let ivt_addr = (vector as u64) * 4;
    let offset = match read_u16_wrapped(state, bus, ivt_addr) {
        Ok(v) => v as u64,
        Err(e) => return deliver_cpu_exception(bus, state, pending, e, saved_rip),
    };
    let segment = match read_u16_wrapped(state, bus, ivt_addr.wrapping_add(2)) {
        Ok(v) => v,
        Err(e) => return deliver_cpu_exception(bus, state, pending, e, saved_rip),
    };

    // Push FLAGS, CS, IP (in that order).
    let flags = state.rflags() as u16;
    let cs = state.segments.cs.selector;
    let ip = saved_rip as u16;

    if push16(bus, state, pending, flags, saved_rip)? == PushOutcome::NestedExceptionDelivered {
        return Ok(());
    }
    if push16(bus, state, pending, cs, saved_rip)? == PushOutcome::NestedExceptionDelivered {
        return Ok(());
    }
    if push16(bus, state, pending, ip, saved_rip)? == PushOutcome::NestedExceptionDelivered {
        return Ok(());
    }

    // Real-mode INT clears IF and TF.
    let new_flags = (state.rflags() & !(RFLAGS_IF | RFLAGS_TF)) | RFLAGS_RESERVED1;
    state.set_rflags(new_flags);

    // Load handler CS:IP.
    state.write_reg(Register::CS, segment as u64);
    state.set_ip(offset);

    pending.interrupt_frames.push(InterruptFrame::Real16);
    Ok(())
}

fn deliver_protected_mode<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    delivery: VectorDelivery,
) -> Result<(), CpuExit> {
    let gate = match with_supervisor_access(bus, state, |bus, state| {
        read_idt_gate32(bus, state, delivery.vector)
    }) {
        Ok(gate) => gate,
        Err(e) => return deliver_cpu_exception(bus, state, pending, e, delivery.saved_rip),
    };
    if !gate.present {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::SegmentNotPresent,
            delivery.saved_rip,
            Some(0),
        );
    }

    if gate.gate_type == GateType::Task {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            delivery.saved_rip,
            Some(0),
        );
    }

    if delivery.is_interrupt
        && delivery.source == InterruptSource::Software
        && state.cpl() > gate.dpl
    {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            delivery.saved_rip,
            Some(0),
        );
    }

    let current_cpl = state.cpl();
    let new_cpl = (gate.selector & 0x3) as u8;
    let old_cs = state.segments.cs.selector;
    let mut stack_switched = false;

    let old_ss = state.segments.ss.selector;
    let old_esp = state.read_gpr32(gpr::RSP);

    if new_cpl < current_cpl {
        let (new_ss_raw, new_esp) = match with_supervisor_access(bus, state, |bus, state| {
            tss32_stack_for_cpl(bus, state, new_cpl)
        }) {
            Ok(stack) => stack,
            Err(e) => return deliver_cpu_exception(bus, state, pending, e, delivery.saved_rip),
        };
        // Hardware forces SS.RPL == CPL for the new stack segment.
        let new_ss = (new_ss_raw & !0b11) | (new_cpl as u16);
        state.segments.ss.selector = new_ss;
        state.write_gpr32(gpr::RSP, new_esp);
        stack_switched = true;

        // Switch to the handler's privilege level before touching the new stack
        // so paging permission checks observe the updated CPL.
        state.segments.cs.selector = gate.selector;
        bus.sync(state);

        // Push old SS:ESP on the new stack.
        if push32(bus, state, pending, old_ss as u32, delivery.saved_rip)?
            == PushOutcome::NestedExceptionDelivered
        {
            return Ok(());
        }
        if push32(bus, state, pending, old_esp, delivery.saved_rip)?
            == PushOutcome::NestedExceptionDelivered
        {
            return Ok(());
        }
    }

    // Push return frame.
    let eflags = state.rflags() as u32;
    if push32(bus, state, pending, eflags, delivery.saved_rip)?
        == PushOutcome::NestedExceptionDelivered
    {
        return Ok(());
    }
    if push32(bus, state, pending, old_cs as u32, delivery.saved_rip)?
        == PushOutcome::NestedExceptionDelivered
    {
        return Ok(());
    }
    if push32(
        bus,
        state,
        pending,
        delivery.saved_rip as u32,
        delivery.saved_rip,
    )? == PushOutcome::NestedExceptionDelivered
    {
        return Ok(());
    }

    if let Some(code) = delivery.error_code {
        if push32(bus, state, pending, code, delivery.saved_rip)?
            == PushOutcome::NestedExceptionDelivered
        {
            return Ok(());
        }
    }

    // Clear IF for interrupt gates; trap gates keep IF.
    let mut new_flags = state.rflags();
    if gate.gate_type == GateType::Interrupt {
        new_flags &= !RFLAGS_IF;
    }
    // Always clear TF on entry (interrupt or trap gate).
    new_flags &= !RFLAGS_TF;
    new_flags |= RFLAGS_RESERVED1;
    state.set_rflags(new_flags);

    state.segments.cs.selector = gate.selector;
    state.set_ip(gate.offset as u64);

    pending
        .interrupt_frames
        .push(InterruptFrame::Protected32 { stack_switched });
    Ok(())
}

fn deliver_long_mode<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    delivery: VectorDelivery,
) -> Result<(), CpuExit> {
    let gate = match with_supervisor_access(bus, state, |bus, state| {
        read_idt_gate64(bus, state, delivery.vector)
    }) {
        Ok(gate) => gate,
        Err(e) => return deliver_cpu_exception(bus, state, pending, e, delivery.saved_rip),
    };
    if !gate.present {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::SegmentNotPresent,
            delivery.saved_rip,
            Some(0),
        );
    }

    if gate.gate_type == GateType::Task {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            delivery.saved_rip,
            Some(0),
        );
    }

    if delivery.is_interrupt
        && delivery.source == InterruptSource::Software
        && state.cpl() > gate.dpl
    {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            delivery.saved_rip,
            Some(0),
        );
    }

    if !is_canonical(gate.offset) {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            delivery.saved_rip,
            Some(0),
        );
    }

    let current_cpl = state.cpl();
    let new_cpl = (gate.selector & 0x3) as u8;
    let old_cs = state.segments.cs.selector;

    let old_rsp = state.read_gpr64(gpr::RSP);
    let old_ss = state.segments.ss.selector;

    let mut used_ist = false;
    if gate.ist != 0 {
        used_ist = true;
        let new_rsp = match with_supervisor_access(bus, state, |bus, state| {
            tss64_ist_stack(bus, state, gate.ist)
        }) {
            Ok(rsp) => {
                if rsp != 0 && is_canonical(rsp) {
                    rsp
                } else {
                    return deliver_exception(
                        bus,
                        state,
                        pending,
                        Exception::InvalidTss,
                        delivery.saved_rip,
                        Some(0),
                    );
                }
            }
            Err(e) => return deliver_cpu_exception(bus, state, pending, e, delivery.saved_rip),
        };
        state.write_gpr64(gpr::RSP, new_rsp);
    } else if new_cpl < current_cpl {
        let new_rsp = match with_supervisor_access(bus, state, |bus, state| {
            tss64_rsp_for_cpl(bus, state, new_cpl)
        }) {
            Ok(rsp) => {
                if rsp != 0 && is_canonical(rsp) {
                    rsp
                } else {
                    return deliver_exception(
                        bus,
                        state,
                        pending,
                        Exception::InvalidTss,
                        delivery.saved_rip,
                        Some(0),
                    );
                }
            }
            Err(e) => return deliver_cpu_exception(bus, state, pending, e, delivery.saved_rip),
        };
        state.write_gpr64(gpr::RSP, new_rsp);
    }

    let stack_switched = used_ist || new_cpl < current_cpl;
    if stack_switched {
        if new_cpl < current_cpl {
            // Switch to the handler's privilege level before touching the new stack
            // so paging permission checks observe the updated CPL.
            state.segments.cs.selector = gate.selector;
            bus.sync(state);
        }

        if push64(bus, state, pending, old_ss as u64, delivery.saved_rip)?
            == PushOutcome::NestedExceptionDelivered
        {
            return Ok(());
        }
        if push64(bus, state, pending, old_rsp, delivery.saved_rip)?
            == PushOutcome::NestedExceptionDelivered
        {
            return Ok(());
        }
        if new_cpl < current_cpl {
            // In IA-32e mode the CPU loads a NULL selector into SS on privilege transition.
            state.segments.ss.selector = 0;
            state.segments.ss.base = 0;
            state.segments.ss.limit = 0xFFFF_FFFF;
            state.segments.ss.access = 0;
        }
    }

    // Push return frame (RFLAGS, CS, RIP, error code).
    let rflags = state.rflags();
    if push64(bus, state, pending, rflags, delivery.saved_rip)?
        == PushOutcome::NestedExceptionDelivered
    {
        return Ok(());
    }
    if push64(bus, state, pending, old_cs as u64, delivery.saved_rip)?
        == PushOutcome::NestedExceptionDelivered
    {
        return Ok(());
    }
    if push64(bus, state, pending, delivery.saved_rip, delivery.saved_rip)?
        == PushOutcome::NestedExceptionDelivered
    {
        return Ok(());
    }

    if let Some(code) = delivery.error_code {
        if push64(bus, state, pending, code as u64, delivery.saved_rip)?
            == PushOutcome::NestedExceptionDelivered
        {
            return Ok(());
        }
    }

    // Clear IF for interrupt gates; trap gates keep IF.
    let mut new_flags = state.rflags();
    if gate.gate_type == GateType::Interrupt {
        new_flags &= !RFLAGS_IF;
    }
    new_flags &= !RFLAGS_TF;
    new_flags |= RFLAGS_RESERVED1;
    state.set_rflags(new_flags);

    state.segments.cs.selector = gate.selector;
    state.set_ip(gate.offset);

    pending
        .interrupt_frames
        .push(InterruptFrame::Long64 { stack_switched });
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IretOutcome {
    Completed,
    ExceptionDelivered,
}

fn iret_real<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
    saved_rip: u64,
) -> Result<IretOutcome, CpuExit> {
    let ip = match pop16(bus, state) {
        Ok(v) => v as u64,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };
    let cs = match pop16(bus, state) {
        Ok(v) => v,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };
    let flags = match pop16(bus, state) {
        Ok(v) => v as u64,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };

    state.write_reg(Register::CS, cs as u64);
    state.set_ip(ip);

    let new_flags = (state.rflags() & !0xFFFF) | (flags & 0xFFFF) | RFLAGS_RESERVED1;
    state.set_rflags(new_flags);
    Ok(IretOutcome::Completed)
}

fn iret_protected<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
    saved_rip: u64,
    stack_switched: bool,
) -> Result<IretOutcome, CpuExit> {
    let new_eip = match pop32(bus, state) {
        Ok(v) => v as u64,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };
    let new_cs = match pop32(bus, state) {
        Ok(v) => v as u16,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };
    let new_eflags = match pop32(bus, state) {
        Ok(v) => v as u64,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };

    let current_cpl = state.cpl();
    let return_cpl = (new_cs & 0x3) as u8;

    if return_cpl < current_cpl {
        // IRET cannot transfer control to a more privileged CPL.
        deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
            Some(0),
        )?;
        return Ok(IretOutcome::ExceptionDelivered);
    }

    let (new_esp, new_ss) = if stack_switched || return_cpl > current_cpl {
        let esp = match pop32(bus, state) {
            Ok(v) => v as u64,
            Err(e) => {
                deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
                return Ok(IretOutcome::ExceptionDelivered);
            }
        };
        let ss = match pop32(bus, state) {
            Ok(v) => v as u16,
            Err(e) => {
                deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
                return Ok(IretOutcome::ExceptionDelivered);
            }
        };
        (Some(esp), Some(ss))
    } else {
        (None, None)
    };

    state.segments.cs.selector = new_cs;
    state.set_ip(new_eip);

    let cur = state.rflags();
    let mut write_mask = 0xFFFF_FFFFu64;
    // Protected-mode IRET applies the same privilege gating as POPF:
    // - IOPL can only change at CPL0.
    // - IF can only change when CPL <= IOPL.
    if current_cpl != 0 {
        write_mask &= !RFLAGS_IOPL_MASK;
    }
    let iopl = ((cur & RFLAGS_IOPL_MASK) >> 12) as u8;
    if current_cpl > iopl {
        write_mask &= !RFLAGS_IF;
    }
    write_mask &= !(RFLAGS_VM | RFLAGS_VIF | RFLAGS_VIP);

    let new_low = (cur & 0xFFFF_FFFF & !write_mask) | (new_eflags & 0xFFFF_FFFF & write_mask);
    let merged = (cur & !0xFFFF_FFFF) | new_low | RFLAGS_RESERVED1;
    state.set_rflags(merged);

    if let (Some(esp), Some(ss)) = (new_esp, new_ss) {
        state.write_gpr32(gpr::RSP, esp as u32);
        state.segments.ss.selector = ss;
    }

    Ok(IretOutcome::Completed)
}

fn iret_long<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
    saved_rip: u64,
    stack_switched: bool,
) -> Result<IretOutcome, CpuExit> {
    let new_rip = match pop64(bus, state) {
        Ok(v) => v,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };
    let new_cs = match pop64(bus, state) {
        Ok(v) => v as u16,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };
    let new_rflags = match pop64(bus, state) {
        Ok(v) => v,
        Err(e) => {
            deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    };

    if !is_canonical(new_rip) {
        // Non-canonical return RIP faults with #GP(0).
        deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
            Some(0),
        )?;
        return Ok(IretOutcome::ExceptionDelivered);
    }

    let current_cpl = state.cpl();
    let return_cpl = (new_cs & 0x3) as u8;

    if return_cpl < current_cpl {
        // IRETQ cannot transfer control to a more privileged CPL.
        deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
            Some(0),
        )?;
        return Ok(IretOutcome::ExceptionDelivered);
    }

    let (new_rsp, new_ss) = if stack_switched || return_cpl > current_cpl {
        let rsp = match pop64(bus, state) {
            Ok(v) => v,
            Err(e) => {
                deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
                return Ok(IretOutcome::ExceptionDelivered);
            }
        };
        let ss = match pop64(bus, state) {
            Ok(v) => v as u16,
            Err(e) => {
                deliver_cpu_exception(bus, state, pending, e, saved_rip)?;
                return Ok(IretOutcome::ExceptionDelivered);
            }
        };
        (Some(rsp), Some(ss))
    } else {
        (None, None)
    };

    if let Some(rsp) = new_rsp {
        if !is_canonical(rsp) {
            // Non-canonical return RSP faults with #GP(0).
            deliver_exception(
                bus,
                state,
                pending,
                Exception::GeneralProtection,
                saved_rip,
                Some(0),
            )?;
            return Ok(IretOutcome::ExceptionDelivered);
        }
    }

    state.segments.cs.selector = new_cs;
    state.set_ip(new_rip);

    let cur = state.rflags();
    let mut write_mask = u64::MAX;
    // IRETQ applies the same privilege gating as POPF/IRET:
    // - IOPL can only change at CPL0.
    // - IF can only change when CPL <= IOPL.
    if current_cpl != 0 {
        write_mask &= !RFLAGS_IOPL_MASK;
    }
    let iopl = ((cur & RFLAGS_IOPL_MASK) >> 12) as u8;
    if current_cpl > iopl {
        write_mask &= !RFLAGS_IF;
    }
    write_mask &= !(RFLAGS_VM | RFLAGS_VIF | RFLAGS_VIP);

    let merged = (cur & !write_mask) | (new_rflags & write_mask) | RFLAGS_RESERVED1;
    state.set_rflags(merged);

    if let (Some(rsp), Some(ss)) = (new_rsp, new_ss) {
        state.write_gpr64(gpr::RSP, rsp);
        state.segments.ss.selector = ss;
    }

    Ok(IretOutcome::Completed)
}

fn read_idt_gate32<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    vector: u8,
) -> Result<IdtGate32, CpuException> {
    let entry_size = 8u64;
    let offset = (vector as u64) * entry_size;
    if offset + (entry_size - 1) > state.tables.idtr.limit as u64 {
        return Err(CpuException::gp0());
    }

    let addr = state.tables.idtr.base + offset;
    let offset_low = read_u16_wrapped(state, bus, addr)? as u32;
    let selector = read_u16_wrapped(state, bus, addr.wrapping_add(2))?;
    let type_attr = bus.read_u8(state.apply_a20(addr.wrapping_add(5)))?;
    let offset_high = read_u16_wrapped(state, bus, addr.wrapping_add(6))? as u32;
    let offset = offset_low | (offset_high << 16);

    let present = (type_attr & 0x80) != 0;
    let dpl = (type_attr >> 5) & 0x3;
    let gate_type = match type_attr & 0x0F {
        0xE => GateType::Interrupt,
        0xF => GateType::Trap,
        0x5 => GateType::Task,
        _ => return Err(CpuException::gp0()),
    };

    Ok(IdtGate32 {
        offset,
        selector,
        gate_type,
        dpl,
        present,
    })
}

fn read_idt_gate64<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    vector: u8,
) -> Result<IdtGate64, CpuException> {
    let entry_size = 16u64;
    let offset = (vector as u64) * entry_size;
    if offset + (entry_size - 1) > state.tables.idtr.limit as u64 {
        return Err(CpuException::gp0());
    }

    let addr = state.tables.idtr.base + offset;
    let offset_low = read_u16_wrapped(state, bus, addr)? as u64;
    let selector = read_u16_wrapped(state, bus, addr.wrapping_add(2))?;
    let ist = bus.read_u8(state.apply_a20(addr.wrapping_add(4)))? & 0x7;
    let type_attr = bus.read_u8(state.apply_a20(addr.wrapping_add(5)))?;
    let offset_mid = read_u16_wrapped(state, bus, addr.wrapping_add(6))? as u64;
    let offset_high = read_u32_wrapped(state, bus, addr.wrapping_add(8))? as u64;
    let offset = offset_low | (offset_mid << 16) | (offset_high << 32);

    let present = (type_attr & 0x80) != 0;
    let dpl = (type_attr >> 5) & 0x3;
    let gate_type = match type_attr & 0x0F {
        0xE => GateType::Interrupt,
        0xF => GateType::Trap,
        0x5 => GateType::Task,
        _ => return Err(CpuException::gp0()),
    };

    Ok(IdtGate64 {
        offset,
        selector,
        gate_type,
        dpl,
        present,
        ist,
    })
}

fn push16<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    value: u16,
    saved_rip: u64,
) -> Result<PushOutcome, CpuExit> {
    let sp = state.read_gpr16(gpr::RSP).wrapping_sub(2);
    state.write_gpr16(gpr::RSP, sp);
    let addr = state.apply_a20(stack_base(state).wrapping_add(sp as u64));
    match write_u16_wrapped(state, bus, addr, value) {
        Ok(()) => Ok(PushOutcome::Pushed),
        Err(e) => deliver_cpu_exception(bus, state, pending, e, saved_rip)
            .map(|()| PushOutcome::NestedExceptionDelivered),
    }
}

fn push32<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    value: u32,
    saved_rip: u64,
) -> Result<PushOutcome, CpuExit> {
    let esp = state.read_gpr32(gpr::RSP).wrapping_sub(4);
    state.write_gpr32(gpr::RSP, esp);
    let addr = state.apply_a20(stack_base(state).wrapping_add(esp as u64));
    match write_u32_wrapped(state, bus, addr, value) {
        Ok(()) => Ok(PushOutcome::Pushed),
        Err(e) => deliver_cpu_exception(bus, state, pending, e, saved_rip)
            .map(|()| PushOutcome::NestedExceptionDelivered),
    }
}

fn push64<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    value: u64,
    saved_rip: u64,
) -> Result<PushOutcome, CpuExit> {
    let rsp = state.read_gpr64(gpr::RSP).wrapping_sub(8);
    state.write_gpr64(gpr::RSP, rsp);
    let addr = state.apply_a20(stack_base(state).wrapping_add(rsp));
    match write_u64_wrapped(state, bus, addr, value) {
        Ok(()) => Ok(PushOutcome::Pushed),
        Err(e) => deliver_cpu_exception(bus, state, pending, e, saved_rip)
            .map(|()| PushOutcome::NestedExceptionDelivered),
    }
}

fn pop16<B: CpuBus>(bus: &mut B, state: &mut state::CpuState) -> Result<u16, CpuException> {
    let sp = state.read_gpr16(gpr::RSP);
    let addr = state.apply_a20(stack_base(state).wrapping_add(sp as u64));
    let value = read_u16_wrapped(state, bus, addr)?;
    state.write_gpr16(gpr::RSP, sp.wrapping_add(2));
    Ok(value)
}

fn pop32<B: CpuBus>(bus: &mut B, state: &mut state::CpuState) -> Result<u32, CpuException> {
    let esp = state.read_gpr32(gpr::RSP);
    let addr = state.apply_a20(stack_base(state).wrapping_add(esp as u64));
    let value = read_u32_wrapped(state, bus, addr)?;
    state.write_gpr32(gpr::RSP, esp.wrapping_add(4));
    Ok(value)
}

fn pop64<B: CpuBus>(bus: &mut B, state: &mut state::CpuState) -> Result<u64, CpuException> {
    let rsp = state.read_gpr64(gpr::RSP);
    let addr = state.apply_a20(stack_base(state).wrapping_add(rsp));
    let value = read_u64_wrapped(state, bus, addr)?;
    state.write_gpr64(gpr::RSP, rsp.wrapping_add(8));
    Ok(value)
}

fn stack_base(state: &state::CpuState) -> u64 {
    state.seg_base_reg(Register::SS)
}

fn is_canonical(addr: u64) -> bool {
    // Canonical if bits 63:48 are sign-extension of bit 47.
    let sign = (addr >> 47) & 1;
    let upper = addr >> 48;
    if sign == 0 {
        upper == 0
    } else {
        upper == 0xFFFF
    }
}

fn tss32_stack_for_cpl<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    cpl: u8,
) -> Result<(u16, u32), CpuException> {
    if state.tables.tr.is_unusable()
        || !state.tables.tr.is_present()
        || (state.tables.tr.selector >> 3) == 0
        || state.tables.tr.s()
        || !matches!(state.tables.tr.typ(), 0x9 | 0xB)
    {
        return Err(CpuException::ts(0));
    }
    if cpl > 2 {
        return Err(CpuException::ts(0));
    }
    let base = state.tables.tr.base;
    let ring_off = (cpl as u64) * 8;
    let esp_off = 4u64 + ring_off;
    let ss_off = 8u64 + ring_off;
    let limit = state.tables.tr.limit as u64;
    if esp_off.checked_add(3).is_none_or(|end| end > limit)
        || ss_off.checked_add(1).is_none_or(|end| end > limit)
    {
        return Err(CpuException::ts(0));
    }
    let esp_addr = base.checked_add(esp_off).ok_or(CpuException::ts(0))?;
    let ss_addr = base.checked_add(ss_off).ok_or(CpuException::ts(0))?;
    let esp = read_u32_wrapped(state, bus, esp_addr)?;
    let ss = read_u16_wrapped(state, bus, ss_addr)?;
    if (ss >> 3) == 0 {
        return Err(CpuException::ts(0));
    }
    Ok((ss, esp))
}

fn tss64_rsp_for_cpl<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    cpl: u8,
) -> Result<u64, CpuException> {
    if state.tables.tr.is_unusable()
        || !state.tables.tr.is_present()
        || (state.tables.tr.selector >> 3) == 0
        || state.tables.tr.s()
        || !matches!(state.tables.tr.typ(), 0x9 | 0xB)
    {
        return Err(CpuException::ts(0));
    }
    if cpl > 2 {
        return Err(CpuException::ts(0));
    }
    let base = state.tables.tr.base;
    let off = 4u64 + (cpl as u64) * 8;
    let limit = state.tables.tr.limit as u64;
    if off.checked_add(7).is_none_or(|end| end > limit) {
        return Err(CpuException::ts(0));
    }
    let addr = base.checked_add(off).ok_or(CpuException::ts(0))?;
    read_u64_wrapped(state, bus, addr)
}

fn tss64_ist_stack<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    ist: u8,
) -> Result<u64, CpuException> {
    if state.tables.tr.is_unusable()
        || !state.tables.tr.is_present()
        || (state.tables.tr.selector >> 3) == 0
        || state.tables.tr.s()
        || !matches!(state.tables.tr.typ(), 0x9 | 0xB)
    {
        return Err(CpuException::ts(0));
    }
    if !(1..=7).contains(&ist) {
        return Err(CpuException::ts(0));
    }
    let base = state.tables.tr.base;
    let off = 0x24u64 + (ist as u64 - 1) * 8;
    let limit = state.tables.tr.limit as u64;
    if off.checked_add(7).is_none_or(|end| end > limit) {
        return Err(CpuException::ts(0));
    }
    let addr = base.checked_add(off).ok_or(CpuException::ts(0))?;
    read_u64_wrapped(state, bus, addr)
}
