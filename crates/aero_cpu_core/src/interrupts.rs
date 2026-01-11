//! Interrupt/exception delivery (IVT/IDT), privilege transitions, and IRET for
//! the Tier-0 CPU model (`state::CpuState` + `mem::CpuBus`).
//!
//! The `state::CpuState` structure is part of the JIT ABI and intentionally
//! contains only architecturally visible state. Any additional bookkeeping
//! needed by the interpreter (pending events, interrupt shadows, exception
//! nesting, external interrupt FIFO, etc) lives in [`PendingEventState`].

use std::collections::VecDeque;

use aero_x86::Register;

use crate::exceptions::{Exception, InterruptSource, PendingEvent};
use crate::mem::CpuBus;
use crate::state::{self, gpr, CpuMode, RFLAGS_IF, RFLAGS_RESERVED1, RFLAGS_TF};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuExit {
    /// Failure to deliver an exception (including #DF) that results in a reset.
    TripleFault,
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
    match (C::of(first), C::of(second)) {
        (C::Contributory, C::Contributory | C::PageFault) => true,
        (C::PageFault, C::Contributory | C::PageFault) => true,
        _ => false,
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

    /// Call after each successfully executed instruction to update the interrupt
    /// shadow state.
    pub fn retire_instruction(&mut self) {
        if self.interrupt_inhibit > 0 {
            self.interrupt_inhibit -= 1;
        }
    }

    /// Whether there is a pending exception/interrupt waiting to be delivered.
    ///
    /// This is primarily used by execution glue (`exec::Vcpu`) to decide whether
    /// calling [`deliver_pending_event`] will actually deliver anything.
    pub fn has_pending_event(&self) -> bool {
        self.pending_event.is_some()
    }
}

/// Convenience wrapper that owns both the JIT ABI state and the non-ABI
/// interrupt bookkeeping.
#[derive(Debug, Default)]
pub struct CpuCore {
    pub state: state::CpuState,
    pub pending: PendingEventState,
}

impl CpuCore {
    pub fn new(mode: CpuMode) -> Self {
        Self {
            state: state::CpuState::new(mode),
            pending: PendingEventState::default(),
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
    let Some(frame) = pending.interrupt_frames.pop() else {
        // No pending frame; on real hardware this would be #GP(0).
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            state.rip(),
            Some(0),
        );
    };

    match frame {
        InterruptFrame::Real16 => iret_real(state, bus),
        InterruptFrame::Protected32 { stack_switched } => {
            iret_protected(state, bus, stack_switched)
        }
        InterruptFrame::Long64 { stack_switched } => iret_long(state, bus, pending, stack_switched),
    }
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
            vector,
            saved_rip,
            None,
            false,
            InterruptSource::External,
        ),
        PendingEvent::Interrupt {
            vector,
            saved_rip,
            source,
        } => deliver_vector(bus, state, pending, vector, saved_rip, None, true, source),
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
        exception.vector(),
        saved_rip,
        code,
        false,
        InterruptSource::External,
    );

    pending.exception_depth = pending.exception_depth.saturating_sub(1);
    pending.delivering_exception = prev_delivering;
    res
}

fn deliver_vector<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    vector: u8,
    saved_rip: u64,
    error_code: Option<u32>,
    is_interrupt: bool,
    source: InterruptSource,
) -> Result<(), CpuExit> {
    match state.mode {
        CpuMode::Real | CpuMode::Vm86 => deliver_real_mode(bus, state, pending, vector, saved_rip),
        CpuMode::Protected => deliver_protected_mode(
            bus,
            state,
            pending,
            vector,
            saved_rip,
            error_code,
            is_interrupt,
            source,
        ),
        CpuMode::Long => deliver_long_mode(
            bus,
            state,
            pending,
            vector,
            saved_rip,
            error_code,
            is_interrupt,
            source,
        ),
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
    let offset = match bus.read_u16(ivt_addr) {
        Ok(v) => v as u64,
        Err(_) => {
            return deliver_exception(
                bus,
                state,
                pending,
                Exception::GeneralProtection,
                saved_rip,
                Some(0),
            )
        }
    };
    let segment = match bus.read_u16(ivt_addr + 2) {
        Ok(v) => v,
        Err(_) => {
            return deliver_exception(
                bus,
                state,
                pending,
                Exception::GeneralProtection,
                saved_rip,
                Some(0),
            )
        }
    };

    // Push FLAGS, CS, IP (in that order).
    let flags = state.rflags() as u16;
    let cs = state.segments.cs.selector;
    let ip = saved_rip as u16;

    push16(bus, state, pending, flags, saved_rip)?;
    push16(bus, state, pending, cs, saved_rip)?;
    push16(bus, state, pending, ip, saved_rip)?;

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
    vector: u8,
    saved_rip: u64,
    error_code: Option<u32>,
    is_interrupt: bool,
    source: InterruptSource,
) -> Result<(), CpuExit> {
    let gate = match with_supervisor_access(bus, state, |bus, state| read_idt_gate32(bus, state, vector))
    {
        Ok(gate) => gate,
        Err(()) => {
            return deliver_exception(
                bus,
                state,
                pending,
                Exception::GeneralProtection,
                saved_rip,
                Some(0),
            )
        }
    };
    if !gate.present {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::SegmentNotPresent,
            saved_rip,
            Some(0),
        );
    }

    if gate.gate_type == GateType::Task {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
            Some(0),
        );
    }

    if is_interrupt && source == InterruptSource::Software {
        if state.cpl() > gate.dpl {
            return deliver_exception(
                bus,
                state,
                pending,
                Exception::GeneralProtection,
                saved_rip,
                Some(0),
            );
        }
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
            Err(()) => return deliver_exception(bus, state, pending, Exception::InvalidTss, saved_rip, Some(0)),
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
        push32(bus, state, pending, old_ss as u32, saved_rip)?;
        push32(bus, state, pending, old_esp, saved_rip)?;
    }

    // Push return frame.
    let eflags = state.rflags() as u32;
    push32(bus, state, pending, eflags, saved_rip)?;
    push32(bus, state, pending, old_cs as u32, saved_rip)?;
    push32(bus, state, pending, saved_rip as u32, saved_rip)?;

    if let Some(code) = error_code {
        push32(bus, state, pending, code, saved_rip)?;
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
    vector: u8,
    saved_rip: u64,
    error_code: Option<u32>,
    is_interrupt: bool,
    source: InterruptSource,
) -> Result<(), CpuExit> {
    let gate = match with_supervisor_access(bus, state, |bus, state| read_idt_gate64(bus, state, vector))
    {
        Ok(gate) => gate,
        Err(()) => {
            return deliver_exception(
                bus,
                state,
                pending,
                Exception::GeneralProtection,
                saved_rip,
                Some(0),
            )
        }
    };
    if !gate.present {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::SegmentNotPresent,
            saved_rip,
            Some(0),
        );
    }

    if gate.gate_type == GateType::Task {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
            Some(0),
        );
    }

    if is_interrupt && source == InterruptSource::Software {
        if state.cpl() > gate.dpl {
            return deliver_exception(
                bus,
                state,
                pending,
                Exception::GeneralProtection,
                saved_rip,
                Some(0),
            );
        }
    }

    if !is_canonical(gate.offset) {
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            saved_rip,
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
            Ok(rsp) if rsp != 0 && is_canonical(rsp) => rsp,
            _ => return deliver_exception(bus, state, pending, Exception::InvalidTss, saved_rip, Some(0)),
        };
        state.write_gpr64(gpr::RSP, new_rsp);
    } else if new_cpl < current_cpl {
        let new_rsp = match with_supervisor_access(bus, state, |bus, state| {
            tss64_rsp_for_cpl(bus, state, new_cpl)
        }) {
            Ok(rsp) if rsp != 0 && is_canonical(rsp) => rsp,
            _ => return deliver_exception(bus, state, pending, Exception::InvalidTss, saved_rip, Some(0)),
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

        push64(bus, state, pending, old_ss as u64, saved_rip)?;
        push64(bus, state, pending, old_rsp, saved_rip)?;
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
    push64(bus, state, pending, rflags, saved_rip)?;
    push64(bus, state, pending, old_cs as u64, saved_rip)?;
    push64(bus, state, pending, saved_rip, saved_rip)?;

    if let Some(code) = error_code {
        push64(bus, state, pending, code as u64, saved_rip)?;
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

fn iret_real<B: CpuBus>(state: &mut state::CpuState, bus: &mut B) -> Result<(), CpuExit> {
    let ip = pop16(bus, state)? as u64;
    let cs = pop16(bus, state)?;
    let flags = pop16(bus, state)? as u64;

    state.write_reg(Register::CS, cs as u64);
    state.set_ip(ip);

    let new_flags = (state.rflags() & !0xFFFF) | (flags & 0xFFFF) | RFLAGS_RESERVED1;
    state.set_rflags(new_flags);
    Ok(())
}

fn iret_protected<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    stack_switched: bool,
) -> Result<(), CpuExit> {
    let new_eip = pop32(bus, state)? as u64;
    let new_cs = pop32(bus, state)? as u16;
    let new_eflags = pop32(bus, state)? as u64;

    let current_cpl = state.cpl();
    let return_cpl = (new_cs & 0x3) as u8;

    let (new_esp, new_ss) = if stack_switched || return_cpl > current_cpl {
        let esp = pop32(bus, state)? as u64;
        let ss = pop32(bus, state)? as u16;
        (Some(esp), Some(ss))
    } else {
        (None, None)
    };

    state.segments.cs.selector = new_cs;
    state.set_ip(new_eip);
    let cur = state.rflags();
    let merged = (cur & !0xFFFF_FFFF) | (new_eflags & 0xFFFF_FFFF) | RFLAGS_RESERVED1;
    state.set_rflags(merged);

    if let (Some(esp), Some(ss)) = (new_esp, new_ss) {
        state.write_gpr32(gpr::RSP, esp as u32);
        state.segments.ss.selector = ss;
    }

    Ok(())
}

fn iret_long<B: CpuBus>(
    state: &mut state::CpuState,
    bus: &mut B,
    pending: &mut PendingEventState,
    stack_switched: bool,
) -> Result<(), CpuExit> {
    let new_rip = pop64(bus, state)?;
    let new_cs = pop64(bus, state)? as u16;
    let new_rflags = pop64(bus, state)?;

    if !is_canonical(new_rip) {
        // Non-canonical return RIP faults with #GP(0).
        return deliver_exception(
            bus,
            state,
            pending,
            Exception::GeneralProtection,
            state.rip(),
            Some(0),
        );
    }

    let current_cpl = state.cpl();
    let return_cpl = (new_cs & 0x3) as u8;

    let (new_rsp, new_ss) = if stack_switched || return_cpl > current_cpl {
        let rsp = pop64(bus, state)?;
        let ss = pop64(bus, state)? as u16;
        (Some(rsp), Some(ss))
    } else {
        (None, None)
    };

    state.segments.cs.selector = new_cs;
    state.set_ip(new_rip);
    state.set_rflags(new_rflags | RFLAGS_RESERVED1);

    if let (Some(rsp), Some(ss)) = (new_rsp, new_ss) {
        state.write_gpr64(gpr::RSP, rsp);
        state.segments.ss.selector = ss;
    }

    Ok(())
}

fn read_idt_gate32<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    vector: u8,
) -> Result<IdtGate32, ()> {
    let entry_size = 8u64;
    let offset = (vector as u64) * entry_size;
    if offset + (entry_size - 1) > state.tables.idtr.limit as u64 {
        return Err(());
    }

    let addr = state.tables.idtr.base + offset;
    let offset_low = bus.read_u16(addr).map_err(|_| ())? as u32;
    let selector = bus.read_u16(addr + 2).map_err(|_| ())?;
    let type_attr = bus.read_u8(addr + 5).map_err(|_| ())?;
    let offset_high = bus.read_u16(addr + 6).map_err(|_| ())? as u32;
    let offset = offset_low | (offset_high << 16);

    let present = (type_attr & 0x80) != 0;
    let dpl = (type_attr >> 5) & 0x3;
    let gate_type = match type_attr & 0x0F {
        0xE => GateType::Interrupt,
        0xF => GateType::Trap,
        0x5 => GateType::Task,
        _ => return Err(()),
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
) -> Result<IdtGate64, ()> {
    let entry_size = 16u64;
    let offset = (vector as u64) * entry_size;
    if offset + (entry_size - 1) > state.tables.idtr.limit as u64 {
        return Err(());
    }

    let addr = state.tables.idtr.base + offset;
    let offset_low = bus.read_u16(addr).map_err(|_| ())? as u64;
    let selector = bus.read_u16(addr + 2).map_err(|_| ())?;
    let ist = bus.read_u8(addr + 4).map_err(|_| ())? & 0x7;
    let type_attr = bus.read_u8(addr + 5).map_err(|_| ())?;
    let offset_mid = bus.read_u16(addr + 6).map_err(|_| ())? as u64;
    let offset_high = bus.read_u32(addr + 8).map_err(|_| ())? as u64;
    let offset = offset_low | (offset_mid << 16) | (offset_high << 32);

    let present = (type_attr & 0x80) != 0;
    let dpl = (type_attr >> 5) & 0x3;
    let gate_type = match type_attr & 0x0F {
        0xE => GateType::Interrupt,
        0xF => GateType::Trap,
        0x5 => GateType::Task,
        _ => return Err(()),
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
) -> Result<(), CpuExit> {
    let sp = state.read_gpr16(gpr::RSP).wrapping_sub(2);
    state.write_gpr16(gpr::RSP, sp);
    let addr = state.apply_a20(stack_base(state).wrapping_add(sp as u64));
    match bus.write_u16(addr, value) {
        Ok(()) => Ok(()),
        Err(_) => deliver_exception(
            bus,
            state,
            pending,
            Exception::StackFault,
            saved_rip,
            Some(0),
        ),
    }
}

fn push32<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    value: u32,
    saved_rip: u64,
) -> Result<(), CpuExit> {
    let esp = state.read_gpr32(gpr::RSP).wrapping_sub(4);
    state.write_gpr32(gpr::RSP, esp);
    let addr = state.apply_a20(stack_base(state).wrapping_add(esp as u64));
    match bus.write_u32(addr, value) {
        Ok(()) => Ok(()),
        Err(_) => deliver_exception(
            bus,
            state,
            pending,
            Exception::StackFault,
            saved_rip,
            Some(0),
        ),
    }
}

fn push64<B: CpuBus>(
    bus: &mut B,
    state: &mut state::CpuState,
    pending: &mut PendingEventState,
    value: u64,
    saved_rip: u64,
) -> Result<(), CpuExit> {
    let rsp = state.read_gpr64(gpr::RSP).wrapping_sub(8);
    state.write_gpr64(gpr::RSP, rsp);
    let addr = state.apply_a20(stack_base(state).wrapping_add(rsp));
    match bus.write_u64(addr, value) {
        Ok(()) => Ok(()),
        Err(_) => deliver_exception(
            bus,
            state,
            pending,
            Exception::StackFault,
            saved_rip,
            Some(0),
        ),
    }
}

fn pop16<B: CpuBus>(bus: &mut B, state: &mut state::CpuState) -> Result<u16, CpuExit> {
    let sp = state.read_gpr16(gpr::RSP);
    let addr = state.apply_a20(stack_base(state).wrapping_add(sp as u64));
    let value = bus.read_u16(addr).map_err(|_| CpuExit::TripleFault)?;
    state.write_gpr16(gpr::RSP, sp.wrapping_add(2));
    Ok(value)
}

fn pop32<B: CpuBus>(bus: &mut B, state: &mut state::CpuState) -> Result<u32, CpuExit> {
    let esp = state.read_gpr32(gpr::RSP);
    let addr = state.apply_a20(stack_base(state).wrapping_add(esp as u64));
    let value = bus.read_u32(addr).map_err(|_| CpuExit::TripleFault)?;
    state.write_gpr32(gpr::RSP, esp.wrapping_add(4));
    Ok(value)
}

fn pop64<B: CpuBus>(bus: &mut B, state: &mut state::CpuState) -> Result<u64, CpuExit> {
    let rsp = state.read_gpr64(gpr::RSP);
    let addr = state.apply_a20(stack_base(state).wrapping_add(rsp));
    let value = bus.read_u64(addr).map_err(|_| CpuExit::TripleFault)?;
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
) -> Result<(u16, u32), ()> {
    if state.tables.tr.is_unusable()
        || !state.tables.tr.is_present()
        || (state.tables.tr.selector >> 3) == 0
        || state.tables.tr.s()
        || !matches!(state.tables.tr.typ(), 0x9 | 0xB)
    {
        return Err(());
    }
    if cpl > 2 {
        return Err(());
    }
    let base = state.tables.tr.base;
    let ring_off = (cpl as u64) * 8;
    let esp_off = 4u64 + ring_off;
    let ss_off = 8u64 + ring_off;
    let limit = state.tables.tr.limit as u64;
    if esp_off.checked_add(3).map_or(true, |end| end > limit)
        || ss_off.checked_add(1).map_or(true, |end| end > limit)
    {
        return Err(());
    }
    let esp_addr = base.checked_add(esp_off).ok_or(())?;
    let ss_addr = base.checked_add(ss_off).ok_or(())?;
    let esp = bus.read_u32(esp_addr).map_err(|_| ())?;
    let ss = bus.read_u16(ss_addr).map_err(|_| ())?;
    if ss == 0 {
        return Err(());
    }
    Ok((ss, esp))
}

fn tss64_rsp_for_cpl<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    cpl: u8,
) -> Result<u64, ()> {
    if state.tables.tr.is_unusable()
        || !state.tables.tr.is_present()
        || (state.tables.tr.selector >> 3) == 0
        || state.tables.tr.s()
        || !matches!(state.tables.tr.typ(), 0x9 | 0xB)
    {
        return Err(());
    }
    if cpl > 2 {
        return Err(());
    }
    let base = state.tables.tr.base;
    let off = 4u64 + (cpl as u64) * 8;
    let limit = state.tables.tr.limit as u64;
    if off.checked_add(7).map_or(true, |end| end > limit) {
        return Err(());
    }
    let addr = base.checked_add(off).ok_or(())?;
    bus.read_u64(addr).map_err(|_| ())
}

fn tss64_ist_stack<B: CpuBus>(
    bus: &mut B,
    state: &state::CpuState,
    ist: u8,
) -> Result<u64, ()> {
    if state.tables.tr.is_unusable()
        || !state.tables.tr.is_present()
        || (state.tables.tr.selector >> 3) == 0
        || state.tables.tr.s()
        || !matches!(state.tables.tr.typ(), 0x9 | 0xB)
    {
        return Err(());
    }
    if !(1..=7).contains(&ist) {
        return Err(());
    }
    let base = state.tables.tr.base;
    let off = 0x24u64 + (ist as u64 - 1) * 8;
    let limit = state.tables.tr.limit as u64;
    if off.checked_add(7).map_or(true, |end| end > limit) {
        return Err(());
    }
    let addr = base.checked_add(off).ok_or(())?;
    bus.read_u64(addr).map_err(|_| ())
}
