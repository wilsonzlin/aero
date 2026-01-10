//! Interrupt/exception delivery (IVT/IDT), privilege transitions, and IRET.

use crate::bus::Bus;
use crate::exceptions::{Exception, InterruptSource, PendingEvent};
use crate::system::{Cpu, CpuMode};

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

impl Cpu {
    /// Inject an external interrupt vector (e.g. from PIC/APIC).
    pub fn inject_external_interrupt(&mut self, vector: u8) {
        self.external_interrupts.push_back(vector);
    }

    /// Poll an interrupt controller and deliver an interrupt if permitted.
    pub fn poll_and_deliver_external_interrupt<B: Bus, C: InterruptController>(
        &mut self,
        bus: &mut B,
        ctrl: &mut C,
    ) -> Result<(), CpuExit> {
        if let Some(vector) = ctrl.poll_interrupt() {
            self.inject_external_interrupt(vector);
        }
        self.deliver_external_interrupt(bus)
    }

    /// Attempt to deliver the next queued external interrupt.
    pub fn deliver_external_interrupt<B: Bus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        if self.pending_event.is_some() {
            // Exceptions/traps/INTn take priority.
            return Ok(());
        }

        if (self.rflags & Cpu::RFLAGS_IF) == 0 {
            return Ok(());
        }

        if self.interrupt_inhibit > 0 {
            return Ok(());
        }

        let Some(vector) = self.external_interrupts.pop_front() else {
            return Ok(());
        };

        let saved_rip = self.rip;
        self.deliver_event(
            bus,
            PendingEvent::Interrupt {
                vector,
                saved_rip,
                source: InterruptSource::External,
            },
            true,
        )
    }

    /// Deliver any pending event (exception, software interrupt, etc).
    pub fn deliver_pending_event<B: Bus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        let Some(event) = self.pending_event.take() else {
            return Ok(());
        };

        let is_interrupt = matches!(event, PendingEvent::Interrupt { .. });
        self.deliver_event(bus, event, is_interrupt)
    }

    fn deliver_event<B: Bus>(
        &mut self,
        bus: &mut B,
        event: PendingEvent,
        is_interrupt: bool,
    ) -> Result<(), CpuExit> {
        match event {
            PendingEvent::Fault {
                exception,
                saved_rip,
                error_code,
            } => self.deliver_exception(bus, exception, saved_rip, error_code),
            PendingEvent::Trap { vector, saved_rip } => self.deliver_vector(
                bus,
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
            } => self.deliver_vector(bus, vector, saved_rip, None, is_interrupt, source),
        }
    }

    fn deliver_exception<B: Bus>(
        &mut self,
        bus: &mut B,
        exception: Exception,
        saved_rip: u64,
        error_code: Option<u32>,
    ) -> Result<(), CpuExit> {
        // Minimal #DF escalation: if we fault while already delivering an exception, raise #DF.
        if self.exception_depth > 0 {
            if exception == Exception::DoubleFault {
                return Err(CpuExit::TripleFault);
            }
            return self.deliver_exception(bus, Exception::DoubleFault, saved_rip, Some(0));
        }

        self.exception_depth = self.exception_depth.saturating_add(1);
        let code = if exception.pushes_error_code() {
            Some(error_code.unwrap_or(0))
        } else {
            None
        };

        let res = self.deliver_vector(
            bus,
            exception.vector(),
            saved_rip,
            code,
            false,
            InterruptSource::External,
        );

        self.exception_depth = self.exception_depth.saturating_sub(1);
        res
    }

    fn deliver_vector<B: Bus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        saved_rip: u64,
        error_code: Option<u32>,
        is_interrupt: bool,
        source: InterruptSource,
    ) -> Result<(), CpuExit> {
        match self.mode {
            CpuMode::Real => self.deliver_real_mode(bus, vector, saved_rip),
            CpuMode::Protected32 => self.deliver_protected_mode(
                bus,
                vector,
                saved_rip,
                error_code,
                is_interrupt,
                source,
            ),
            CpuMode::Long64 => self.deliver_long_mode(
                bus,
                vector,
                saved_rip,
                error_code,
                is_interrupt,
                source,
            ),
        }
    }

    fn deliver_real_mode<B: Bus>(&mut self, bus: &mut B, vector: u8, saved_rip: u64) -> Result<(), CpuExit> {
        let ivt_addr = (vector as u64) * 4;
        let offset = bus.read_u16(ivt_addr) as u64;
        let segment = bus.read_u16(ivt_addr + 2);

        // Push FLAGS, CS, IP (in that order).
        self.push16(bus, (self.rflags & 0xFFFF) as u16);
        self.push16(bus, self.cs);
        self.push16(bus, (saved_rip & 0xFFFF) as u16);

        // Clear IF and TF.
        const IF_MASK: u64 = 1 << 9;
        const TF_MASK: u64 = 1 << 8;
        self.rflags &= !(IF_MASK | TF_MASK);
        self.rflags |= Cpu::RFLAGS_FIXED1;

        self.cs = segment;
        self.rip = offset;

        Ok(())
    }

    fn deliver_protected_mode<B: Bus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        saved_rip: u64,
        error_code: Option<u32>,
        is_interrupt: bool,
        source: InterruptSource,
    ) -> Result<(), CpuExit> {
        let gate = self.read_idt_gate32(bus, vector).map_err(|_| CpuExit::TripleFault)?;
        if !gate.present {
            return Err(CpuExit::TripleFault);
        }

        if gate.gate_type == GateType::Task {
            return self.deliver_exception(bus, Exception::GeneralProtection, saved_rip, Some(0));
        }

        if is_interrupt && source == InterruptSource::Software {
            if self.cpl() > gate.dpl {
                return self.deliver_exception(bus, Exception::GeneralProtection, saved_rip, Some(0));
            }
        }

        let current_cpl = self.cpl();
        let new_cpl = (gate.selector & 0x3) as u8;

        if new_cpl < current_cpl {
            let tss = self.tss32.ok_or(CpuExit::TripleFault)?;
            let (new_ss, new_esp) = tss.stack_for_cpl(new_cpl).ok_or(CpuExit::TripleFault)?;

            let old_ss = self.ss;
            let old_esp = self.rsp as u32;

            self.ss = new_ss;
            self.rsp = new_esp as u64;

            // Push old SS:ESP on the new stack.
            self.push32(bus, old_ss as u32);
            self.push32(bus, old_esp);
        }

        self.push32(bus, (self.rflags & 0xFFFF_FFFF) as u32);
        self.push32(bus, self.cs as u32);
        self.push32(bus, saved_rip as u32);

        if let Some(code) = error_code {
            self.push32(bus, code);
        }

        // Clear IF for interrupt gates; trap gates keep IF.
        if gate.gate_type == GateType::Interrupt {
            self.rflags &= !Cpu::RFLAGS_IF;
        }
        // Always clear TF on entry (interrupt or trap gate).
        const TF_MASK: u64 = 1 << 8;
        self.rflags &= !TF_MASK;
        self.rflags |= Cpu::RFLAGS_FIXED1;

        self.cs = gate.selector;
        self.rip = gate.offset as u64;

        Ok(())
    }

    fn deliver_long_mode<B: Bus>(
        &mut self,
        bus: &mut B,
        vector: u8,
        saved_rip: u64,
        error_code: Option<u32>,
        is_interrupt: bool,
        source: InterruptSource,
    ) -> Result<(), CpuExit> {
        let gate = self.read_idt_gate64(bus, vector).map_err(|_| CpuExit::TripleFault)?;
        if !gate.present {
            return Err(CpuExit::TripleFault);
        }

        if gate.gate_type == GateType::Task {
            return self.deliver_exception(bus, Exception::GeneralProtection, saved_rip, Some(0));
        }

        if is_interrupt && source == InterruptSource::Software {
            if self.cpl() > gate.dpl {
                return self.deliver_exception(bus, Exception::GeneralProtection, saved_rip, Some(0));
            }
        }

        let current_cpl = self.cpl();
        let new_cpl = (gate.selector & 0x3) as u8;

        let old_rsp = self.rsp;
        let old_ss = self.ss;

        // Stack switching (IST has priority over CPL-based stacks).
        if gate.ist != 0 {
            let tss = self.tss64.ok_or(CpuExit::TripleFault)?;
            let new_rsp = tss.ist_stack(gate.ist).ok_or(CpuExit::TripleFault)?;
            self.rsp = new_rsp;
        } else if new_cpl < current_cpl {
            let tss = self.tss64.ok_or(CpuExit::TripleFault)?;
            let new_rsp = tss.rsp_for_cpl(new_cpl).ok_or(CpuExit::TripleFault)?;
            self.rsp = new_rsp;
        }

        // Push return frame.
        if new_cpl < current_cpl {
            self.push64(bus, old_ss as u64);
            self.push64(bus, old_rsp);
            // In IA-32e mode the CPU loads a NULL selector into SS on privilege transition.
            self.ss = 0;
        }

        self.push64(bus, self.rflags);
        self.push64(bus, self.cs as u64);
        self.push64(bus, saved_rip);

        if let Some(code) = error_code {
            self.push64(bus, code as u64);
        }

        // Clear IF for interrupt gates; trap gates keep IF.
        if gate.gate_type == GateType::Interrupt {
            self.rflags &= !Cpu::RFLAGS_IF;
        }
        // Always clear TF on entry (interrupt or trap gate).
        const TF_MASK: u64 = 1 << 8;
        self.rflags &= !TF_MASK;
        self.rflags |= Cpu::RFLAGS_FIXED1;

        self.cs = gate.selector;
        self.rip = gate.offset;

        Ok(())
    }

    /// Execute an IRET/IRETD/IRETQ depending on the current mode.
    pub fn iret<B: Bus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        match self.mode {
            CpuMode::Real => self.iret_real(bus),
            CpuMode::Protected32 => self.iret_protected(bus),
            CpuMode::Long64 => self.iret_long(bus),
        }
    }

    fn iret_real<B: Bus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        let ip = self.pop16(bus) as u64;
        let cs = self.pop16(bus);
        let flags = self.pop16(bus) as u64;

        self.rip = ip;
        self.cs = cs;
        self.rflags = (self.rflags & !0xFFFF) | (flags & 0xFFFF) | Cpu::RFLAGS_FIXED1;

        Ok(())
    }

    fn iret_protected<B: Bus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        let new_eip = self.pop32(bus) as u64;
        let new_cs = self.pop32(bus) as u16;
        let new_eflags = self.pop32(bus) as u64;

        let return_cpl = (new_cs & 0x3) as u8;
        let current_cpl = self.cpl();

        self.rip = new_eip;
        self.cs = new_cs;
        self.rflags = (self.rflags & !0xFFFF_FFFF) | (new_eflags & 0xFFFF_FFFF) | Cpu::RFLAGS_FIXED1;

        if return_cpl > current_cpl {
            let new_esp = self.pop32(bus) as u64;
            let new_ss = self.pop32(bus) as u16;
            self.rsp = new_esp;
            self.ss = new_ss;
        }

        Ok(())
    }

    fn iret_long<B: Bus>(&mut self, bus: &mut B) -> Result<(), CpuExit> {
        let new_rip = self.pop64(bus);
        let new_cs = self.pop64(bus) as u16;
        let new_rflags = self.pop64(bus);

        let return_cpl = (new_cs & 0x3) as u8;
        let current_cpl = self.cpl();

        self.rip = new_rip;
        self.cs = new_cs;
        self.rflags = new_rflags | Cpu::RFLAGS_FIXED1;

        if return_cpl > current_cpl {
            let new_rsp = self.pop64(bus);
            let new_ss = self.pop64(bus) as u16;
            self.rsp = new_rsp;
            self.ss = new_ss;
        }

        Ok(())
    }

    fn read_idt_gate32<B: Bus>(&self, bus: &mut B, vector: u8) -> Result<IdtGate32, ()> {
        let entry_size = 8u64;
        let offset = (vector as u64) * entry_size;
        if offset + (entry_size - 1) > self.idtr.limit as u64 {
            return Err(());
        }

        let addr = self.idtr.base + offset;
        let offset_low = bus.read_u16(addr) as u32;
        let selector = bus.read_u16(addr + 2);
        let type_attr = bus.read_u8(addr + 5);
        let offset_high = bus.read_u16(addr + 6) as u32;
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

    fn read_idt_gate64<B: Bus>(&self, bus: &mut B, vector: u8) -> Result<IdtGate64, ()> {
        let entry_size = 16u64;
        let offset = (vector as u64) * entry_size;
        if offset + (entry_size - 1) > self.idtr.limit as u64 {
            return Err(());
        }

        let addr = self.idtr.base + offset;
        let offset_low = bus.read_u16(addr) as u64;
        let selector = bus.read_u16(addr + 2);
        let ist = bus.read_u8(addr + 4) & 0x7;
        let type_attr = bus.read_u8(addr + 5);
        let offset_mid = bus.read_u16(addr + 6) as u64;
        let offset_high = bus.read_u32(addr + 8) as u64;
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

    fn push16<B: Bus>(&mut self, bus: &mut B, value: u16) {
        let sp = (self.rsp as u16).wrapping_sub(2);
        self.rsp = (self.rsp & !0xFFFF) | sp as u64;
        let addr = ((self.ss as u64) << 4) + sp as u64;
        bus.write_u16(addr, value);
    }

    fn push32<B: Bus>(&mut self, bus: &mut B, value: u32) {
        let esp = (self.rsp as u32).wrapping_sub(4);
        self.rsp = (self.rsp & !0xFFFF_FFFF) | esp as u64;
        bus.write_u32(self.rsp, value);
    }

    fn push64<B: Bus>(&mut self, bus: &mut B, value: u64) {
        self.rsp = self.rsp.wrapping_sub(8);
        bus.write_u64(self.rsp, value);
    }

    fn pop16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let sp = self.rsp as u16;
        let addr = ((self.ss as u64) << 4) + sp as u64;
        let value = bus.read_u16(addr);
        let new_sp = sp.wrapping_add(2);
        self.rsp = (self.rsp & !0xFFFF) | new_sp as u64;
        value
    }

    fn pop32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let value = bus.read_u32(self.rsp);
        let esp = (self.rsp as u32).wrapping_add(4);
        self.rsp = (self.rsp & !0xFFFF_FFFF) | esp as u64;
        value
    }

    fn pop64<B: Bus>(&mut self, bus: &mut B) -> u64 {
        let value = bus.read_u64(self.rsp);
        self.rsp = self.rsp.wrapping_add(8);
        value
    }
}

