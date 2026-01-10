#![forbid(unsafe_code)]

use aero_core::memory::Memory;

use crate::descriptors::{
    parse_idt_gate_descriptor_32, parse_idt_gate_descriptor_64, parse_real_mode_idt_entry,
    GateSize, GateType, IdtGateDescriptor,
};
use crate::state::{CpuState, Exception, Gpr, SegReg, CR0_PE, RFLAGS_IF, RFLAGS_TF};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptSource {
    Software,
    External,
    Exception,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFaultErrorCode(u32);

impl PageFaultErrorCode {
    pub fn new(
        present: bool,
        write: bool,
        user: bool,
        reserved_bit_violation: bool,
        instruction_fetch: bool,
    ) -> Self {
        let mut code = 0u32;
        if present {
            code |= 1 << 0;
        }
        if write {
            code |= 1 << 1;
        }
        if user {
            code |= 1 << 2;
        }
        if reserved_bit_violation {
            code |= 1 << 3;
        }
        if instruction_fetch {
            code |= 1 << 4;
        }
        Self(code)
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackPtrWidth {
    Bits16,
    Bits32,
    Bits64,
}

impl StackPtrWidth {
    fn wrap_mask(self) -> u64 {
        match self {
            StackPtrWidth::Bits16 => 0xFFFF,
            StackPtrWidth::Bits32 => 0xFFFF_FFFF,
            StackPtrWidth::Bits64 => 0xFFFF_FFFF_FFFF_FFFF,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushWidth {
    Bits16,
    Bits32,
    Bits64,
}

impl PushWidth {
    fn bytes(self) -> u64 {
        match self {
            PushWidth::Bits16 => 2,
            PushWidth::Bits32 => 4,
            PushWidth::Bits64 => 8,
        }
    }

    fn mask(self) -> u64 {
        match self {
            PushWidth::Bits16 => 0xFFFF,
            PushWidth::Bits32 => 0xFFFF_FFFF,
            PushWidth::Bits64 => 0xFFFF_FFFF_FFFF_FFFF,
        }
    }
}

impl CpuState {
    fn stack_ptr_width(&self) -> StackPtrWidth {
        if self.long_mode_active() {
            return StackPtrWidth::Bits64;
        }
        if self.control.cr0 & CR0_PE == 0 {
            return StackPtrWidth::Bits16;
        }
        // protected mode: SS.D/B selects 16 vs 32 stack pointer width
        if self.ss.cache.flags & 0b0100 != 0 {
            StackPtrWidth::Bits32
        } else {
            StackPtrWidth::Bits16
        }
    }

    fn push_width_from_gate(gate: &IdtGateDescriptor) -> PushWidth {
        match gate.size {
            GateSize::Bits16 => PushWidth::Bits16,
            GateSize::Bits32 => PushWidth::Bits32,
            GateSize::Bits64 => PushWidth::Bits64,
        }
    }

    fn push_value<M: Memory>(
        &mut self,
        mem: &mut M,
        sp_width: StackPtrWidth,
        push_width: PushWidth,
        value: u64,
    ) -> Result<(), Exception> {
        let bytes = push_width.bytes();
        let mask = sp_width.wrap_mask();
        let sp = self.gpr64(Gpr::Rsp) & mask;
        let new_sp = sp.wrapping_sub(bytes) & mask;

        self.set_gpr64(Gpr::Rsp, (self.gpr64(Gpr::Rsp) & !mask) | new_sp);

        let addr = self.ss.cache.base.wrapping_add(new_sp);
        let value = value & push_width.mask();

        let res = match push_width {
            PushWidth::Bits16 => mem.write_u16(addr, value as u16),
            PushWidth::Bits32 => mem.write_u32(addr, value as u32),
            PushWidth::Bits64 => mem.write_u64(addr, value),
        };
        res.map_err(|_| Exception::GeneralProtection { code: 0 })?;
        Ok(())
    }

    pub fn software_interrupt<M: Memory>(
        &mut self,
        mem: &mut M,
        vector: u8,
    ) -> Result<(), Exception> {
        self.deliver_interrupt(mem, vector, InterruptSource::Software, None)
    }

    pub fn external_interrupt<M: Memory>(
        &mut self,
        mem: &mut M,
        vector: u8,
    ) -> Result<(), Exception> {
        if !self.rflags.if_flag() {
            return Ok(());
        }
        self.deliver_interrupt(mem, vector, InterruptSource::External, None)
    }

    pub fn raise_page_fault<M: Memory>(
        &mut self,
        mem: &mut M,
        addr: u64,
        code: PageFaultErrorCode,
    ) -> Result<(), Exception> {
        self.control.cr2 = addr;
        self.deliver_interrupt(mem, 14, InterruptSource::Exception, Some(code.bits()))
    }

    pub fn raise_general_protection<M: Memory>(
        &mut self,
        mem: &mut M,
        code: u32,
    ) -> Result<(), Exception> {
        self.deliver_interrupt(mem, 13, InterruptSource::Exception, Some(code))
    }

    pub fn deliver_exception<M: Memory>(
        &mut self,
        mem: &mut M,
        exception: Exception,
    ) -> Result<(), Exception> {
        match exception {
            Exception::PageFault { addr, code } => {
                self.control.cr2 = addr;
                self.deliver_interrupt(mem, 14, InterruptSource::Exception, Some(code))
            }
            other => self.deliver_interrupt(
                mem,
                other.vector(),
                InterruptSource::Exception,
                other.error_code(),
            ),
        }
    }

    fn deliver_interrupt<M: Memory>(
        &mut self,
        mem: &mut M,
        vector: u8,
        source: InterruptSource,
        error_code: Option<u32>,
    ) -> Result<(), Exception> {
        if self.is_real_mode() {
            return self.deliver_interrupt_real_mode(mem, vector, error_code);
        }

        if self.long_mode_active() {
            self.deliver_interrupt_long_mode(mem, vector, source, error_code)
        } else {
            self.deliver_interrupt_protected_mode(mem, vector, source, error_code)
        }
    }

    fn deliver_interrupt_real_mode<M: Memory>(
        &mut self,
        mem: &mut M,
        vector: u8,
        error_code: Option<u32>,
    ) -> Result<(), Exception> {
        let offset = (vector as u64) * 4;
        if !self.idtr.contains(offset, 4) {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        let mut entry_bytes = [0u8; 4];
        mem.read(self.idtr.base + offset, &mut entry_bytes)
            .map_err(|_| Exception::GeneralProtection { code: 0 })?;
        let entry = parse_real_mode_idt_entry(entry_bytes);

        let old_flags = self.rflags.read();
        let old_cs = self.cs.selector;
        let old_ip = self.ip();

        let sp_width = StackPtrWidth::Bits16;
        let push_width = PushWidth::Bits16;
        self.push_value(mem, sp_width, push_width, old_flags)?;
        self.push_value(mem, sp_width, push_width, old_cs as u64)?;
        self.push_value(mem, sp_width, push_width, old_ip as u64)?;
        if let Some(code) = error_code {
            self.push_value(mem, sp_width, push_width, code as u64)?;
        }

        // Real-mode INT clears IF and TF.
        let flags = old_flags & !(RFLAGS_IF | RFLAGS_TF);
        self.rflags.set_raw(flags);

        self.set_segment_real_mode(SegReg::Cs, entry.segment);
        self.set_ip(entry.offset);
        Ok(())
    }

    fn fetch_idt_gate_descriptor_protected_mode<M: Memory>(
        &self,
        mem: &M,
        vector: u8,
    ) -> Result<IdtGateDescriptor, Exception> {
        let offset = (vector as u64) * 8;
        if !self.idtr.contains(offset, 8) {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        let mut bytes = [0u8; 8];
        mem.read(self.idtr.base + offset, &mut bytes)
            .map_err(|_| Exception::GeneralProtection { code: 0 })?;
        parse_idt_gate_descriptor_32(bytes).map_err(|_| Exception::GeneralProtection { code: 0 })
    }

    fn fetch_idt_gate_descriptor_long_mode<M: Memory>(
        &self,
        mem: &M,
        vector: u8,
    ) -> Result<IdtGateDescriptor, Exception> {
        let offset = (vector as u64) * 16;
        if !self.idtr.contains(offset, 16) {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        let mut bytes = [0u8; 16];
        mem.read(self.idtr.base + offset, &mut bytes)
            .map_err(|_| Exception::GeneralProtection { code: 0 })?;
        parse_idt_gate_descriptor_64(bytes).map_err(|_| Exception::GeneralProtection { code: 0 })
    }

    fn deliver_interrupt_protected_mode<M: Memory>(
        &mut self,
        mem: &mut M,
        vector: u8,
        source: InterruptSource,
        error_code: Option<u32>,
    ) -> Result<(), Exception> {
        let gate = self.fetch_idt_gate_descriptor_protected_mode(mem, vector)?;
        if !gate.present {
            return Err(Exception::GeneralProtection {
                code: (vector as u32) << 3,
            });
        }
        if source == InterruptSource::Software && self.cpl() > gate.dpl {
            return Err(Exception::GeneralProtection {
                code: (vector as u32) << 3,
            });
        }

        let old_cpl = self.cpl();
        let old_flags = self.rflags.read();
        let old_cs = self.cs.selector;
        let old_ip = if matches!(gate.size, GateSize::Bits16) {
            self.ip() as u64
        } else {
            self.eip() as u64
        };
        let old_ss = self.ss.selector;
        let old_sp = match self.stack_ptr_width() {
            StackPtrWidth::Bits16 => (self.gpr64(Gpr::Rsp) as u16) as u64,
            StackPtrWidth::Bits32 => (self.gpr64(Gpr::Rsp) as u32) as u64,
            StackPtrWidth::Bits64 => self.gpr64(Gpr::Rsp),
        };

        let handler_selector = gate.selector;
        let cs_desc = self.fetch_segment_descriptor(handler_selector, mem)?;
        if !cs_desc.is_present() || !cs_desc.is_code() {
            return Err(Exception::GeneralProtection {
                code: handler_selector as u32,
            });
        }

        let new_cpl = if cs_desc.code_conforming() {
            old_cpl
        } else {
            cs_desc.dpl()
        };

        if new_cpl > old_cpl {
            return Err(Exception::GeneralProtection {
                code: handler_selector as u32,
            });
        }

        // Install handler CS (updates CPL), so subsequent SS checks use the target CPL.
        self.cs = crate::state::SegmentRegister {
            selector: (handler_selector & !0x3) | (new_cpl as u16),
            cache: self.build_segment_cache(SegReg::Cs, &cs_desc),
        };

        // Stack switch on privilege change.
        if new_cpl < old_cpl {
            let (new_ss, new_sp) = self.read_tss_stack_ptr_protected(mem, new_cpl)?;
            self.load_segment(SegReg::Ss, new_ss, mem)?;
            self.set_gpr32(Gpr::Rsp, new_sp);
        }

        // Determine stack pointer width after any stack switch.
        let sp_width = self.stack_ptr_width();
        let push_width = Self::push_width_from_gate(&gate);

        if new_cpl < old_cpl {
            self.push_value(mem, sp_width, push_width, old_ss as u64)?;
            self.push_value(mem, sp_width, push_width, old_sp)?;
        }

        self.push_value(mem, sp_width, push_width, old_flags)?;
        self.push_value(mem, sp_width, push_width, old_cs as u64)?;
        self.push_value(mem, sp_width, push_width, old_ip)?;
        if let Some(code) = error_code {
            self.push_value(mem, sp_width, push_width, code as u64)?;
        }

        if gate.gate_type == GateType::Interrupt {
            self.rflags.set_raw(old_flags & !RFLAGS_IF);
        }

        match gate.size {
            GateSize::Bits16 => self.set_ip(gate.offset as u16),
            GateSize::Bits32 => self.set_eip(gate.offset as u32),
            GateSize::Bits64 => unreachable!("64-bit gate in protected mode"),
        }

        Ok(())
    }

    fn deliver_interrupt_long_mode<M: Memory>(
        &mut self,
        mem: &mut M,
        vector: u8,
        source: InterruptSource,
        error_code: Option<u32>,
    ) -> Result<(), Exception> {
        let gate = self.fetch_idt_gate_descriptor_long_mode(mem, vector)?;
        if !gate.present {
            return Err(Exception::GeneralProtection {
                code: (vector as u32) << 3,
            });
        }
        if source == InterruptSource::Software && self.cpl() > gate.dpl {
            return Err(Exception::GeneralProtection {
                code: (vector as u32) << 3,
            });
        }

        let old_cpl = self.cpl();
        let old_flags = self.rflags.read();
        let old_cs = self.cs.selector;
        let old_rip = self.rip;
        let old_ss = self.ss.selector;
        let old_rsp = self.gpr64(Gpr::Rsp);

        let cs_desc = self.fetch_segment_descriptor(gate.selector, mem)?;
        if !cs_desc.is_present() || !cs_desc.is_code() {
            return Err(Exception::GeneralProtection {
                code: gate.selector as u32,
            });
        }
        if cs_desc.long() && cs_desc.default_operand_size_32() {
            return Err(Exception::GeneralProtection {
                code: gate.selector as u32,
            });
        }

        let new_cpl = if cs_desc.code_conforming() {
            old_cpl
        } else {
            cs_desc.dpl()
        };

        if new_cpl > old_cpl {
            return Err(Exception::GeneralProtection {
                code: gate.selector as u32,
            });
        }

        self.cs = crate::state::SegmentRegister {
            selector: (gate.selector & !0x3) | (new_cpl as u16),
            cache: self.build_segment_cache(SegReg::Cs, &cs_desc),
        };

        // Stack switch on CPL change or IST.
        let mut used_ist = false;
        if gate.ist != 0 {
            used_ist = true;
            let new_rsp = self.read_tss_ist_long_mode(mem, gate.ist)?;
            self.set_gpr64(Gpr::Rsp, new_rsp);
        } else if new_cpl < old_cpl {
            let new_rsp = self.read_tss_rsp_long_mode(mem, new_cpl)?;
            self.set_gpr64(Gpr::Rsp, new_rsp);
        }

        let sp_width = StackPtrWidth::Bits64;
        let push_width = PushWidth::Bits64;

        if used_ist || new_cpl < old_cpl {
            self.push_value(mem, sp_width, push_width, old_ss as u64)?;
            self.push_value(mem, sp_width, push_width, old_rsp)?;
        }

        self.push_value(mem, sp_width, push_width, old_flags)?;
        self.push_value(mem, sp_width, push_width, old_cs as u64)?;
        self.push_value(mem, sp_width, push_width, old_rip)?;
        if let Some(code) = error_code {
            self.push_value(mem, sp_width, push_width, code as u64)?;
        }

        if gate.gate_type == GateType::Interrupt {
            self.rflags.set_raw(old_flags & !RFLAGS_IF);
        }

        self.rip = gate.offset;
        Ok(())
    }

    fn read_tss_stack_ptr_protected<M: Memory>(
        &self,
        mem: &M,
        cpl: u8,
    ) -> Result<(u16, u32), Exception> {
        if self.tr.selector & 0xFFFC == 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }

        let base = self.tr.base;
        let (esp_off, ss_off) = match cpl {
            0 => (4u64, 8u64),
            1 => (12u64, 16u64),
            2 => (20u64, 24u64),
            _ => return Err(Exception::GeneralProtection { code: 0 }),
        };

        let esp = mem
            .read_u32(base + esp_off)
            .map_err(|_| Exception::GeneralProtection { code: 0 })?;
        let ss = mem
            .read_u16(base + ss_off)
            .map_err(|_| Exception::GeneralProtection { code: 0 })?;
        Ok((ss, esp))
    }

    fn read_tss_rsp_long_mode<M: Memory>(&self, mem: &M, cpl: u8) -> Result<u64, Exception> {
        if self.tr.selector & 0xFFFC == 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        let base = self.tr.base;
        let off = match cpl {
            0 => 4u64,
            1 => 12u64,
            2 => 20u64,
            _ => return Err(Exception::GeneralProtection { code: 0 }),
        };
        mem.read_u64(base + off)
            .map_err(|_| Exception::GeneralProtection { code: 0 })
    }

    fn read_tss_ist_long_mode<M: Memory>(&self, mem: &M, ist: u8) -> Result<u64, Exception> {
        if self.tr.selector & 0xFFFC == 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        if !(1..=7).contains(&ist) {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        let base = self.tr.base;
        let off = 36u64 + ((ist as u64 - 1) * 8);
        mem.read_u64(base + off)
            .map_err(|_| Exception::GeneralProtection { code: 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msr::{EFER_LME, IA32_EFER};
    use crate::state::{CR0_PG, CR4_PAE};
    use aero_core::memory::VecMemory;

    fn write_desc(mem: &mut VecMemory, addr: u64, desc: u64) {
        mem.write_u64(addr, desc).unwrap();
    }

    fn write_u128(mem: &mut VecMemory, addr: u64, val: u128) {
        mem.write_u64(addr, val as u64).unwrap();
        mem.write_u64(addr + 8, (val >> 64) as u64).unwrap();
    }

    #[test]
    fn page_fault_error_code_bits() {
        let code = PageFaultErrorCode::new(true, true, true, false, true);
        assert_eq!(code.bits() & 0x1F, 0b1_0_1_1_1);
    }

    #[test]
    fn page_fault_updates_cr2_and_pushes_error_code() {
        let mut mem = VecMemory::new(0x4000);
        let mut cpu = CpuState::default();

        // Setup protected mode with a minimal GDT.
        let gdt_base = 0x1000;
        // null descriptor
        write_desc(&mut mem, gdt_base, 0);
        // code segment (index 1): base=0, limit=0xfffff, G=1, D=1, present.
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF);
        // data segment (index 2): base=0, limit=0xfffff, G=1, D=1, present.
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);

        cpu.lgdt(gdt_base, 0x17);
        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap();
        cpu.load_segment(SegReg::Ss, 0x10, &mem).unwrap();
        cpu.set_gpr32(Gpr::Rsp, 0x2000);

        // IDT with a 32-bit interrupt gate for vector 14.
        let idt_base = 0x1800;
        let handler_offset = 0x12345678u32;
        let gate: u64 = (handler_offset as u64 & 0xFFFF)
            | ((0x08u64) << 16)
            | (0u64 << 32)
            | (0x8Eu64 << 40)
            | (((handler_offset as u64) & 0xFFFF_0000) << 32);
        write_desc(&mut mem, idt_base + (14 * 8) as u64, gate);
        cpu.lidt(idt_base, 0x0FFF);

        let code = PageFaultErrorCode::new(false, true, false, false, false);
        cpu.raise_page_fault(&mut mem, 0xDEADBEEF, code).unwrap();

        assert_eq!(cpu.control.cr2, 0xDEADBEEF);

        // Stack grows down; error code should be at the top.
        let sp = cpu.gpr32(Gpr::Rsp) as u64;
        let pushed_error = mem.read_u32(cpu.ss.cache.base + sp).unwrap();
        assert_eq!(pushed_error, code.bits());
    }

    #[test]
    fn real_mode_interrupt_pushes_16bit_frame_and_clears_if_tf() {
        let mut mem = VecMemory::new(0x40000);
        let mut cpu = CpuState::default();

        cpu.set_segment_real_mode(SegReg::Cs, 0x1000);
        cpu.set_segment_real_mode(SegReg::Ss, 0x2000);
        cpu.set_ip(0x0100);
        cpu.set_gpr16(Gpr::Rsp, 0xFFFE);
        cpu.rflags.set_raw((1 << 1) | RFLAGS_IF | RFLAGS_TF);

        // IVT entry for vector 0x10: handler at 0x3000:0x0200.
        let entry_addr = 0x10u64 * 4;
        mem.write_u16(entry_addr, 0x0200).unwrap();
        mem.write_u16(entry_addr + 2, 0x3000).unwrap();

        cpu.software_interrupt(&mut mem, 0x10).unwrap();

        assert_eq!(cpu.cs.selector, 0x3000);
        assert_eq!(cpu.ip(), 0x0200);
        assert!(!cpu.rflags.if_flag());
        assert_eq!(cpu.rflags.read() & RFLAGS_TF, 0);

        // FLAGS, CS, IP pushed as 16-bit values.
        let sp = cpu.gpr16(Gpr::Rsp);
        assert_eq!(sp, 0xFFF8);
        let base = cpu.ss.cache.base + sp as u64;
        let pushed_ip = mem.read_u16(base).unwrap();
        let pushed_cs = mem.read_u16(base + 2).unwrap();
        let pushed_flags = mem.read_u16(base + 4).unwrap();
        assert_eq!(pushed_ip, 0x0100);
        assert_eq!(pushed_cs, 0x1000);
        assert_eq!(pushed_flags & (RFLAGS_IF as u16), RFLAGS_IF as u16);
    }

    #[test]
    fn protected_mode_interrupt_gate_pushes_32bit_frame_and_clears_if() {
        let mut mem = VecMemory::new(0x8000);
        let mut cpu = CpuState::default();

        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        cpu.lgdt(gdt_base, 0x17);
        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap();
        cpu.load_segment(SegReg::Ss, 0x10, &mem).unwrap();

        cpu.set_eip(0xCAFEBABE);
        cpu.set_gpr32(Gpr::Rsp, 0x3000);
        cpu.rflags.set_raw((1 << 1) | RFLAGS_IF);

        let idt_base = 0x2000;
        let handler_offset = 0x12345678u32;
        let gate: u64 = (handler_offset as u64 & 0xFFFF)
            | ((0x08u64) << 16)
            | (0u64 << 32)
            | (0x8Eu64 << 40)
            | (((handler_offset as u64) & 0xFFFF_0000) << 32);
        write_desc(&mut mem, idt_base + (0x80 * 8) as u64, gate);
        cpu.lidt(idt_base, 0x0FFF);

        cpu.software_interrupt(&mut mem, 0x80).unwrap();

        assert_eq!(cpu.eip(), handler_offset);
        assert_eq!(cpu.cs.selector, 0x08);
        assert!(!cpu.rflags.if_flag());

        let sp = cpu.gpr32(Gpr::Rsp) as u64;
        assert_eq!(sp, 0x3000 - 12);
        let base = cpu.ss.cache.base + sp;
        let pushed_eip = mem.read_u32(base).unwrap();
        let pushed_cs = mem.read_u32(base + 4).unwrap();
        let pushed_eflags = mem.read_u32(base + 8).unwrap();
        assert_eq!(pushed_eip, 0xCAFEBABE);
        assert_eq!(pushed_cs & 0xFFFF, 0x08);
        assert_ne!(pushed_eflags & (RFLAGS_IF as u32), 0);
    }

    #[test]
    fn protected_mode_trap_gate_does_not_clear_if() {
        let mut mem = VecMemory::new(0x8000);
        let mut cpu = CpuState::default();

        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        cpu.lgdt(gdt_base, 0x17);
        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap();
        cpu.load_segment(SegReg::Ss, 0x10, &mem).unwrap();

        cpu.set_eip(0x11112222);
        cpu.set_gpr32(Gpr::Rsp, 0x3000);
        cpu.rflags.set_raw((1 << 1) | RFLAGS_IF);

        let idt_base = 0x2000;
        let handler_offset = 0x33334444u32;
        let gate: u64 = (handler_offset as u64 & 0xFFFF)
            | ((0x08u64) << 16)
            | (0u64 << 32)
            | (0x8Fu64 << 40) // present trap gate
            | (((handler_offset as u64) & 0xFFFF_0000) << 32);
        write_desc(&mut mem, idt_base + (0x81 * 8) as u64, gate);
        cpu.lidt(idt_base, 0x0FFF);

        cpu.software_interrupt(&mut mem, 0x81).unwrap();
        assert!(cpu.rflags.if_flag());
    }

    #[test]
    fn protected_mode_interrupt_from_ring3_switches_to_tss_stack() {
        let mut mem = VecMemory::new(0x20000);
        let mut cpu = CpuState::default();

        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        // kernel code/data
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        // user code/data (DPL=3)
        write_desc(&mut mem, gdt_base + 24, 0x00CFFA000000FFFF);
        write_desc(&mut mem, gdt_base + 32, 0x00CFF2000000FFFF);

        // 32-bit available TSS descriptor at index 5.
        let tss_base = 0x1800u32;
        let tss_limit = 0x67u32;
        let tss_desc: u64 = (tss_limit as u64 & 0xFFFF)
            | ((tss_base as u64 & 0xFFFF) << 16)
            | (((tss_base as u64 >> 16) & 0xFF) << 32)
            | (0x89u64 << 40)
            | (((tss_limit as u64 >> 16) & 0xF) << 48)
            | (((tss_base as u64 >> 24) & 0xFF) << 56);
        write_desc(&mut mem, gdt_base + 40, tss_desc);

        cpu.lgdt(gdt_base, 0x2F);
        cpu.write_cr0(cpu.control.cr0 | CR0_PE);

        // Set CPL=3 before loading user segments.
        cpu.cs.selector = 0x1B;
        cpu.load_segment(SegReg::Cs, 0x1B, &mem).unwrap();
        cpu.load_segment(SegReg::Ss, 0x23, &mem).unwrap();
        cpu.set_gpr32(Gpr::Rsp, 0x9000);
        cpu.set_eip(0x44445555);

        // Fill TSS.ss0/esp0.
        mem.write_u32(tss_base as u64 + 4, 0x8000).unwrap();
        mem.write_u16(tss_base as u64 + 8, 0x10).unwrap();
        cpu.ltr(0x28, &mem).unwrap();

        // IDT gate with DPL=3 so INT is allowed from ring 3.
        let idt_base = 0x2000;
        let handler_offset = 0x11223344u32;
        let gate: u64 = (handler_offset as u64 & 0xFFFF)
            | ((0x08u64) << 16)
            | (0u64 << 32)
            | (0xEEu64 << 40)
            | (((handler_offset as u64) & 0xFFFF_0000) << 32);
        write_desc(&mut mem, idt_base + (0x30 * 8) as u64, gate);
        cpu.lidt(idt_base, 0x0FFF);

        cpu.software_interrupt(&mut mem, 0x30).unwrap();

        // Now in ring 0, on the ring 0 stack.
        assert_eq!(cpu.cpl(), 0);
        assert_eq!(cpu.cs.selector, 0x08);
        assert_eq!(cpu.ss.selector, 0x10);
        assert_eq!(cpu.eip(), handler_offset);

        let sp = cpu.gpr32(Gpr::Rsp) as u64;
        assert_eq!(sp, 0x8000 - 20);
        let base = cpu.ss.cache.base + sp;
        let pushed_eip = mem.read_u32(base).unwrap();
        let pushed_cs = mem.read_u32(base + 4).unwrap();
        let pushed_eflags = mem.read_u32(base + 8).unwrap();
        let pushed_esp = mem.read_u32(base + 12).unwrap();
        let pushed_ss = mem.read_u32(base + 16).unwrap();

        assert_eq!(pushed_eip, 0x44445555);
        assert_eq!(pushed_cs & 0xFFFF, 0x1B);
        assert_eq!(pushed_esp, 0x9000);
        assert_eq!(pushed_ss & 0xFFFF, 0x23);
        assert_ne!(pushed_eflags & (1 << 1), 0);
    }

    #[test]
    fn long_mode_interrupt_pushes_64bit_frame() {
        let mut mem = VecMemory::new(0x20000);
        let mut cpu = CpuState::default();

        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00AF9A000000FFFF); // 64-bit code
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF); // data
        cpu.lgdt(gdt_base, 0x17);

        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.write_cr4(cpu.control.cr4 | CR4_PAE);
        cpu.write_msr(IA32_EFER, cpu.msrs.efer | EFER_LME).unwrap();
        cpu.write_cr0(cpu.control.cr0 | CR0_PG);
        assert!(cpu.long_mode_active());

        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap();
        cpu.rip = 0x1111_2222_3333_4444;
        cpu.set_gpr64(Gpr::Rsp, 0x9000);
        cpu.rflags.set_raw((1 << 1) | RFLAGS_IF);

        let idt_base = 0x1800;
        let handler_offset = 0xAAAABBBBCCCCDDDDu64;
        let gate = (handler_offset & 0xFFFF)
            | ((0x08u64) << 16)
            | ((0u64) << 32) // IST
            | (0x8Eu64 << 40)
            | (((handler_offset >> 16) & 0xFFFF) << 48);
        let gate_high = (handler_offset >> 32) as u32 as u64;
        let gate_u128 = (gate as u128) | ((gate_high as u128) << 64);
        write_u128(&mut mem, idt_base + (0x20 * 16) as u64, gate_u128);
        cpu.lidt(idt_base, 0x0FFF);

        cpu.software_interrupt(&mut mem, 0x20).unwrap();
        assert_eq!(cpu.rip, handler_offset);
        assert!(!cpu.rflags.if_flag());

        let rsp = cpu.gpr64(Gpr::Rsp);
        assert_eq!(rsp, 0x9000 - 24);
        let base = cpu.ss.cache.base + rsp;
        let pushed_rip = mem.read_u64(base).unwrap();
        let pushed_cs = mem.read_u64(base + 8).unwrap();
        let pushed_rflags = mem.read_u64(base + 16).unwrap();
        assert_eq!(pushed_rip, 0x1111_2222_3333_4444);
        assert_eq!(pushed_cs & 0xFFFF, 0x08);
        assert_ne!(pushed_rflags & RFLAGS_IF, 0);
    }
}
