use aero_cpu_core::interrupts::{CpuCore, CpuExit, InterruptController};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode, RFLAGS_IF, RFLAGS_IOPL_MASK, SEG_ACCESS_PRESENT};
use aero_x86::Register;

fn write_idt_gate32(
    mem: &mut impl CpuBus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 8;
    mem.write_u16(addr, (offset & 0xFFFF) as u16).unwrap();
    mem.write_u16(addr + 2, selector).unwrap();
    mem.write_u8(addr + 4, 0).unwrap();
    mem.write_u8(addr + 5, type_attr).unwrap();
    mem.write_u16(addr + 6, (offset >> 16) as u16).unwrap();
}

fn write_idt_gate64(
    mem: &mut impl CpuBus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u64,
    ist: u8,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 16;
    mem.write_u16(addr, (offset & 0xFFFF) as u16).unwrap();
    mem.write_u16(addr + 2, selector).unwrap();
    mem.write_u8(addr + 4, ist & 0x7).unwrap();
    mem.write_u8(addr + 5, type_attr).unwrap();
    mem.write_u16(addr + 6, ((offset >> 16) & 0xFFFF) as u16)
        .unwrap();
    mem.write_u32(addr + 8, ((offset >> 32) & 0xFFFF_FFFF) as u32)
        .unwrap();
    mem.write_u32(addr + 12, 0).unwrap();
}

#[test]
fn int_real_mode_uses_ivt_and_pushes_frame() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    // IVT[0x10] = 2222:1111
    mem.write_u16(0x10 * 4, 0x1111).unwrap();
    mem.write_u16(0x10 * 4 + 2, 0x2222).unwrap();

    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.state.write_reg(Register::CS, 0x1234);
    cpu.state.write_reg(Register::SS, 0x2000);
    cpu.state.write_reg(Register::SP, 0xFFFE);
    cpu.state.set_rflags(0x202); // IF=1

    cpu.pending.raise_software_interrupt(0x10, 0x5678);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x2222);
    assert_eq!(cpu.state.rip(), 0x1111);
    assert_eq!(cpu.state.read_reg(Register::SP) as u16, 0xFFF8);
    assert_eq!(cpu.state.rflags() & RFLAGS_IF, 0); // IF cleared

    let stack_base = (0x2000u64) << 4;
    assert_eq!(mem.read_u16(stack_base + 0xFFF8).unwrap(), 0x5678); // IP
    assert_eq!(mem.read_u16(stack_base + 0xFFFA).unwrap(), 0x1234); // CS
    assert_eq!(mem.read_u16(stack_base + 0xFFFC).unwrap(), 0x0202); // FLAGS

    Ok(())
}

#[test]
fn int_protected_mode_no_privilege_change_pushes_eflags_cs_eip() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x10000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x80, 0x08, 0x2000, 0x8E); // present, DPL0, int gate

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x1000);
    cpu.state.set_rflags(0x202);

    cpu.pending.raise_software_interrupt(0x80, 0x1234);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.rip(), 0x2000);
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), 0x0FF4);
    assert_eq!(cpu.state.rflags() & RFLAGS_IF, 0); // IF cleared by interrupt gate

    assert_eq!(mem.read_u32(0x0FF4).unwrap(), 0x1234); // EIP
    assert_eq!(mem.read_u32(0x0FF8).unwrap(), 0x08); // CS
    assert_eq!(mem.read_u32(0x0FFC).unwrap(), 0x202); // EFLAGS

    Ok(())
}

#[test]
fn hlt_is_cleared_when_an_external_interrupt_is_delivered() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x10000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x20, 0x08, 0x2000, 0x8E); // present, DPL0, int gate

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x1000);
    cpu.state.set_rflags(0x202); // IF=1
    cpu.state.halted = true;

    cpu.pending.inject_external_interrupt(0x20);
    cpu.deliver_external_interrupt(&mut mem)?;

    assert!(
        !cpu.state.halted,
        "CPU should wake on delivered external interrupt"
    );
    assert_eq!(cpu.state.rip(), 0x2000);

    Ok(())
}

#[test]
fn int_protected_mode_cpl3_to_cpl0_stack_switch_and_iret_restore() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x80, 0x08, 0x3000, 0xEE); // present, DPL3, int gate

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.state.segments.ss.selector = 0x23;
    cpu.state.write_gpr32(gpr::RSP, 0x8000);
    cpu.state.set_rflags(0x202);

    let tss_base = 0x18000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    // 32-bit TSS: ESP0 at +4, SS0 at +8.
    mem.write_u32(tss_base + 4, 0x9000).unwrap();
    mem.write_u16(tss_base + 8, 0x10).unwrap();

    cpu.pending.raise_software_interrupt(0x80, 0x0040_0000);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0x10);
    assert_eq!(cpu.state.rip(), 0x3000);
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), 0x8FEC);

    // New stack frame (top -> bottom): EIP, CS, EFLAGS, old ESP, old SS.
    assert_eq!(mem.read_u32(0x8FEC).unwrap(), 0x0040_0000);
    assert_eq!(mem.read_u32(0x8FF0).unwrap(), 0x1B);
    assert_eq!(mem.read_u32(0x8FF4).unwrap(), 0x202);
    assert_eq!(mem.read_u32(0x8FF8).unwrap(), 0x8000);
    assert_eq!(mem.read_u32(0x8FFC).unwrap(), 0x23);

    cpu.iret(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x1B);
    assert_eq!(cpu.state.segments.ss.selector, 0x23);
    assert_eq!(cpu.state.rip(), 0x0040_0000);
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), 0x8000);
    assert_ne!(cpu.state.rflags() & RFLAGS_IF, 0); // IF restored

    Ok(())
}

#[test]
fn page_fault_sets_cr2_and_pushes_error_code() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 14, 0x08, 0x4000, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x2000);
    cpu.state.set_rflags(0x202);

    cpu.pending.raise_exception_fault(
        &mut cpu.state,
        aero_cpu_core::exceptions::Exception::PageFault,
        0x1234_5678,
        Some(0xDEAD),
        Some(0xCAFE_BABE),
    );
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.control.cr2, 0xCAFE_BABE);
    assert_eq!(cpu.state.rip(), 0x4000);
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), 0x1FF0);

    // top -> bottom: error_code, eip, cs, eflags
    assert_eq!(mem.read_u32(0x1FF0).unwrap(), 0xDEAD);
    assert_eq!(mem.read_u32(0x1FF4).unwrap(), 0x1234_5678);
    assert_eq!(mem.read_u32(0x1FF8).unwrap(), 0x08);
    assert_eq!(mem.read_u32(0x1FFC).unwrap(), 0x202);

    Ok(())
}

#[derive(Debug)]
struct FailingWriteU32Bus {
    inner: FlatTestBus,
    remaining_write_u32_failures: usize,
}

impl FailingWriteU32Bus {
    fn new(size: usize, write_u32_failures: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            remaining_write_u32_failures: write_u32_failures,
        }
    }
}

impl CpuBus for FailingWriteU32Bus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception> {
        self.inner.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, aero_cpu_core::Exception> {
        self.inner.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, aero_cpu_core::Exception> {
        self.inner.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, aero_cpu_core::Exception> {
        self.inner.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, aero_cpu_core::Exception> {
        self.inner.read_u128(vaddr)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), aero_cpu_core::Exception> {
        if self.remaining_write_u32_failures > 0 {
            self.remaining_write_u32_failures -= 1;
            // Simulate a write-intent page fault at the destination address.
            return Err(aero_cpu_core::Exception::PageFault {
                addr: vaddr,
                error_code: 0x2,
            });
        }
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), aero_cpu_core::Exception> {
        self.inner.write_u128(vaddr, val)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception> {
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), aero_cpu_core::Exception> {
        self.inner.io_write(port, size, val)
    }
}

#[test]
fn page_fault_delivery_failure_escalates_to_double_fault() -> Result<(), CpuExit> {
    // Fail the first 32-bit stack push while delivering #PF to force a nested #PF,
    // which should escalate to #DF.
    let mut mem = FailingWriteU32Bus::new(0x20000, 1);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 14, 0x08, 0x4000, 0x8E);
    write_idt_gate32(&mut mem, idt_base, 8, 0x08, 0x5000, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x2000);
    cpu.state.set_rflags(0x202);

    cpu.pending.raise_exception_fault(
        &mut cpu.state,
        aero_cpu_core::exceptions::Exception::PageFault,
        0x1234,
        Some(0xDEAD),
        Some(0xCAFE_BABE),
    );
    cpu.deliver_pending_event(&mut mem)?;

    // CR2 should contain the faulting address of the nested #PF raised during delivery.
    assert_eq!(cpu.state.control.cr2, 0x1FFC);
    assert_eq!(cpu.state.rip(), 0x5000);

    // Stack frame for #DF: error_code, eip, cs, eflags.
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), 0x1FEC);
    assert_eq!(mem.read_u32(0x1FEC).unwrap(), 0);
    assert_eq!(mem.read_u32(0x1FF0).unwrap(), 0x1234);
    assert_eq!(mem.read_u32(0x1FF4).unwrap(), 0x08);
    assert_eq!(mem.read_u32(0x1FF8).unwrap(), 0x202);

    Ok(())
}

#[test]
fn double_fault_delivery_failure_triggers_triple_fault() {
    // Fail the first stack push while delivering #PF (forcing #DF) and then fail the
    // first stack push while delivering #DF, which should trigger a triple fault.
    let mut mem = FailingWriteU32Bus::new(0x20000, 2);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 14, 0x08, 0x4000, 0x8E);
    write_idt_gate32(&mut mem, idt_base, 8, 0x08, 0x5000, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x2000);
    cpu.state.set_rflags(0x202);

    cpu.pending.raise_exception_fault(
        &mut cpu.state,
        aero_cpu_core::exceptions::Exception::PageFault,
        0x1234,
        Some(0xDEAD),
        Some(0xCAFE_BABE),
    );

    assert_eq!(
        cpu.deliver_pending_event(&mut mem),
        Err(CpuExit::TripleFault)
    );
}

#[test]
fn sti_shadow_blocks_immediate_external_interrupt_delivery() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x20, 0x08, 0x5555, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x3000);
    cpu.state.set_rip(0x1111);
    cpu.state.set_rflags(0x202); // IF=1

    // STI shadow for one instruction.
    cpu.pending.inhibit_interrupts_for_one_instruction();

    cpu.pending.inject_external_interrupt(0x20);
    cpu.deliver_external_interrupt(&mut mem)?;
    // Not delivered because of STI shadow.
    assert_eq!(cpu.state.rip(), 0x1111);
    assert_eq!(cpu.pending.external_interrupts.len(), 1);

    // Age the STI interrupt shadow.
    cpu.pending.retire_instruction();

    cpu.deliver_external_interrupt(&mut mem)?;
    assert_eq!(cpu.state.rip(), 0x5555);
    assert_eq!(cpu.pending.external_interrupts.len(), 0);
    Ok(())
}

#[test]
fn mov_ss_shadow_blocks_immediate_external_interrupt_delivery() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x20, 0x08, 0x7777, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x3000);
    cpu.state.set_rip(0x1111);
    cpu.state.set_rflags(0x202); // IF=1

    // Model MOV SS / POP SS interrupt shadow (does not touch IF).
    cpu.pending.inhibit_interrupts_for_one_instruction();

    cpu.pending.inject_external_interrupt(0x20);
    cpu.deliver_external_interrupt(&mut mem)?;
    assert_eq!(cpu.state.rip(), 0x1111);
    assert_eq!(cpu.pending.external_interrupts.len(), 1);

    cpu.pending.retire_instruction();
    cpu.deliver_external_interrupt(&mut mem)?;
    assert_eq!(cpu.state.rip(), 0x7777);
    assert_eq!(cpu.pending.external_interrupts.len(), 0);

    Ok(())
}

#[test]
fn int_long_mode_cpl3_to_cpl0_uses_rsp0_and_iretq_returns() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    write_idt_gate64(&mut mem, idt_base, 0x80, 0x08, 0x5000, 0, 0xEE);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // user code (CPL3)
    cpu.state.segments.ss.selector = 0x2B; // user data
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    // 64-bit TSS: RSP0 at +4.
    mem.write_u64(tss_base + 4, 0x9000).unwrap();

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.rip(), 0x5000);
    assert_eq!(cpu.state.read_gpr64(gpr::RSP), 0x9000 - 40);
    assert_eq!(cpu.state.rflags() & RFLAGS_IF, 0); // IF cleared on entry

    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    assert_eq!(mem.read_u64(frame_base).unwrap(), 0x4000_0010); // RIP
    assert_eq!(mem.read_u64(frame_base + 8).unwrap(), 0x33); // CS
    assert_eq!(mem.read_u64(frame_base + 16).unwrap(), 0x202); // RFLAGS
    assert_eq!(mem.read_u64(frame_base + 24).unwrap(), 0x7000); // RSP
    assert_eq!(mem.read_u64(frame_base + 32).unwrap(), 0x2B); // SS

    cpu.iret(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x33);
    assert_eq!(cpu.state.segments.ss.selector, 0x2B);
    assert_eq!(cpu.state.rip(), 0x4000_0010);
    assert_eq!(cpu.state.read_gpr64(gpr::RSP), 0x7000);
    assert_ne!(cpu.state.rflags() & RFLAGS_IF, 0); // IF restored

    Ok(())
}

#[test]
fn int_long_mode_ist_overrides_rsp0() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    write_idt_gate64(&mut mem, idt_base, 0x81, 0x08, 0x6000, 1, 0xEE);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // user code (CPL3)
    cpu.state.segments.ss.selector = 0x2B; // user data
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    mem.write_u64(tss_base + 4, 0x9000).unwrap();
    // 64-bit TSS: IST1 at +0x24.
    mem.write_u64(tss_base + 0x24, 0xA000).unwrap();

    cpu.pending.raise_software_interrupt(0x81, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    // IST1 overrides RSP0.
    assert_eq!(cpu.state.read_gpr64(gpr::RSP), 0xA000 - 40);

    cpu.iret(&mut mem)?;
    assert_eq!(cpu.state.rip(), 0x4000_0010);
    assert_eq!(cpu.state.read_gpr64(gpr::RSP), 0x7000);
    assert_eq!(cpu.state.segments.ss.selector, 0x2B);
    Ok(())
}

#[test]
fn int_long_mode_non_canonical_rsp0_delivers_ts_using_ist() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    // Software interrupt gate (DPL3 so CPL3 can invoke it).
    write_idt_gate64(&mut mem, idt_base, 0x80, 0x08, 0x5000, 0, 0xEE);
    // #TS handler uses IST1 so it can be delivered even when RSP0 is invalid.
    write_idt_gate64(&mut mem, idt_base, 10, 0x08, 0x6000, 1, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // user code (CPL3)
    cpu.state.segments.ss.selector = 0x2B; // user data
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    // Non-canonical RSP0 should raise #TS.
    mem.write_u64(tss_base + 4, 0x0001_0000_0000_0000).unwrap();
    // IST1 stack for #TS delivery.
    mem.write_u64(tss_base + 0x24, 0x9000).unwrap();

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.rip(), 0x6000);
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0);
    assert_eq!(cpu.state.rflags() & RFLAGS_IF, 0);

    // Stack frame for #TS (IST1): error_code, RIP, CS, RFLAGS, old RSP, old SS.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    assert_eq!(frame_base, 0x9000 - 48);
    assert_eq!(mem.read_u64(frame_base).unwrap(), 0); // error_code
    assert_eq!(mem.read_u64(frame_base + 8).unwrap(), 0x4000_0010); // RIP
    assert_eq!(mem.read_u64(frame_base + 16).unwrap(), 0x33); // CS
    assert_ne!(mem.read_u64(frame_base + 24).unwrap() & RFLAGS_IF, 0); // saved RFLAGS
    assert_eq!(mem.read_u64(frame_base + 32).unwrap(), 0x7000); // old RSP
    assert_eq!(mem.read_u64(frame_base + 40).unwrap(), 0x2B); // old SS

    Ok(())
}

#[test]
fn int_long_mode_non_canonical_ist_pointer_delivers_ts_using_alt_ist() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    // Software interrupt gate uses IST1 (DPL3 so CPL3 can invoke it).
    write_idt_gate64(&mut mem, idt_base, 0x81, 0x08, 0x5000, 1, 0xEE);
    // #TS handler uses IST2 so it can be delivered even when IST1 is invalid.
    write_idt_gate64(&mut mem, idt_base, 10, 0x08, 0x6000, 2, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // user code (CPL3)
    cpu.state.segments.ss.selector = 0x2B; // user data
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    // Provide a valid RSP0 (not used because the interrupt uses IST1).
    mem.write_u64(tss_base + 4, 0x9000).unwrap();
    // Non-canonical IST1 should raise #TS.
    mem.write_u64(tss_base + 0x24, 0x0001_0000_0000_0000)
        .unwrap();
    // IST2 stack for #TS delivery.
    mem.write_u64(tss_base + 0x2C, 0x9000).unwrap();

    cpu.pending.raise_software_interrupt(0x81, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.rip(), 0x6000);
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0);
    assert_eq!(cpu.state.rflags() & RFLAGS_IF, 0);

    // Stack frame for #TS (IST2): error_code, RIP, CS, RFLAGS, old RSP, old SS.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    assert_eq!(frame_base, 0x9000 - 48);
    assert_eq!(mem.read_u64(frame_base).unwrap(), 0); // error_code
    assert_eq!(mem.read_u64(frame_base + 8).unwrap(), 0x4000_0010); // RIP
    assert_eq!(mem.read_u64(frame_base + 16).unwrap(), 0x33); // CS
    assert_ne!(mem.read_u64(frame_base + 24).unwrap() & RFLAGS_IF, 0); // saved RFLAGS
    assert_eq!(mem.read_u64(frame_base + 32).unwrap(), 0x7000); // old RSP
    assert_eq!(mem.read_u64(frame_base + 40).unwrap(), 0x2B); // old SS

    Ok(())
}

#[test]
fn iretq_long_mode_non_canonical_rsp_delivers_gp() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    write_idt_gate64(&mut mem, idt_base, 0x80, 0x08, 0x5000, 0, 0xEE);
    // #GP handler at 0x6000.
    write_idt_gate64(&mut mem, idt_base, 13, 0x08, 0x6000, 0, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // user code (CPL3)
    cpu.state.segments.ss.selector = 0x2B; // user data
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    mem.write_u64(tss_base + 4, 0x9000).unwrap();

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    // Corrupt the saved RSP so `IRETQ` would restore a non-canonical stack pointer.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    mem.write_u64(frame_base + 24, 0x0001_0000_0000_0000)
        .unwrap();

    cpu.iret(&mut mem)?;

    // The `IRETQ` should fault with #GP(0) before committing the non-canonical RSP.
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.rip(), 0x6000);
    assert_eq!(cpu.state.segments.ss.selector, 0);
    assert_ne!(cpu.state.read_gpr64(gpr::RSP), 0x0001_0000_0000_0000);

    Ok(())
}

#[test]
fn iret_protected_mode_cannot_return_to_more_privileged_cpl() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    // INT 0x80 stays at CPL3 so we can exercise an attempted privilege escalation via IRET.
    write_idt_gate32(&mut mem, idt_base, 0x80, 0x1B, 0x5000, 0xEE);
    // #GP handler at 0x6000 (CPL0).
    write_idt_gate32(&mut mem, idt_base, 13, 0x08, 0x6000, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.state.segments.ss.selector = 0x23;
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr32(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    // Provide a ring-0 stack for #GP delivery.
    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    mem.write_u32(tss_base + 4, 0x9000).unwrap();
    mem.write_u16(tss_base + 8, 0x10).unwrap();

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x1B);
    assert_eq!(cpu.state.rip(), 0x5000);

    // Corrupt the saved CS so IRET would attempt to return from CPL3 to CPL0.
    let frame_base = cpu.state.read_gpr32(gpr::RSP) as u64;
    mem.write_u32(frame_base + 4, 0x08).unwrap();

    cpu.iret(&mut mem)?;

    // The IRET should fault with #GP(0) instead of returning to CPL0.
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.rip(), 0x6000);

    Ok(())
}

#[test]
fn iret_protected_mode_does_not_restore_iopl_from_user_frame() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    // Deliver an interrupt to a CPL3 handler so IRET executes at CPL3.
    write_idt_gate32(&mut mem, idt_base, 0x80, 0x1B, 0x5000, 0xEE);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.state.segments.ss.selector = 0x23;
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr32(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202); // IF=1, IOPL=0

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    // Corrupt the saved EFLAGS to attempt raising IOPL=3.
    let frame_base = cpu.state.read_gpr32(gpr::RSP) as u64;
    mem.write_u32(frame_base + 8, 0x202 | (3 << 12)).unwrap();

    cpu.iret(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x1B);
    assert_eq!(cpu.state.rip(), 0x4000_0010);
    assert_eq!(cpu.state.rflags() & RFLAGS_IOPL_MASK, 0);

    Ok(())
}

#[test]
fn iretq_long_mode_cannot_return_to_more_privileged_cpl() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    // INT 0x80 stays at CPL3 so we can exercise an attempted privilege escalation via IRETQ.
    write_idt_gate64(&mut mem, idt_base, 0x80, 0x33, 0x5000, 0, 0xEE);
    // #GP handler at 0x6000 (CPL0).
    write_idt_gate64(&mut mem, idt_base, 13, 0x08, 0x6000, 0, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // user code (CPL3)
    cpu.state.segments.ss.selector = 0x2B; // user data
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    // Provide a ring-0 stack for #GP delivery.
    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    mem.write_u64(tss_base + 4, 0x9000).unwrap();

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x33);
    assert_eq!(cpu.state.rip(), 0x5000);

    // Corrupt the saved CS so IRETQ would attempt to return from CPL3 to CPL0.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    mem.write_u64(frame_base + 8, 0x08).unwrap();

    cpu.iret(&mut mem)?;

    // The IRETQ should fault with #GP(0) instead of returning to CPL0.
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.rip(), 0x6000);

    Ok(())
}

#[test]
fn iretq_long_mode_does_not_restore_iopl_from_user_frame() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    // Deliver an interrupt to a CPL3 handler so IRETQ executes at CPL3.
    write_idt_gate64(&mut mem, idt_base, 0x80, 0x33, 0x5000, 0, 0xEE);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // CPL3
    cpu.state.segments.ss.selector = 0x2B;
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202); // IF=1, IOPL=0

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    // Corrupt the saved RFLAGS to attempt raising IOPL=3.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    mem.write_u64(frame_base + 16, 0x202 | (3 << 12)).unwrap();

    cpu.iret(&mut mem)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x33);
    assert_eq!(cpu.state.rip(), 0x4000_0010);
    assert_eq!(cpu.state.rflags() & RFLAGS_IOPL_MASK, 0);

    Ok(())
}

#[test]
fn iretq_long_mode_non_canonical_rip_delivers_gp() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    write_idt_gate64(&mut mem, idt_base, 0x80, 0x08, 0x5000, 0, 0xEE);
    // #GP handler at 0x6000.
    write_idt_gate64(&mut mem, idt_base, 13, 0x08, 0x6000, 0, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x33; // user code (CPL3)
    cpu.state.segments.ss.selector = 0x2B; // user data
    cpu.state.set_rip(0x4000_0000);
    cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.state.set_rflags(0x202);

    let tss_base = 0x10000u64;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    mem.write_u64(tss_base + 4, 0x9000).unwrap();

    cpu.pending.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    // Corrupt the saved RIP so `IRETQ` would restore a non-canonical instruction pointer.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    mem.write_u64(frame_base, 0x0001_0000_0000_0000).unwrap();

    cpu.iret(&mut mem)?;

    // The `IRETQ` should fault with #GP(0) before committing the non-canonical RIP.
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.rip(), 0x6000);
    assert_eq!(cpu.state.segments.ss.selector, 0);
    assert_ne!(cpu.state.rip(), 0x0001_0000_0000_0000);

    Ok(())
}

struct OneShotController(Option<u8>);

impl InterruptController for OneShotController {
    fn poll_interrupt(&mut self) -> Option<u8> {
        self.0.take()
    }
}

#[test]
fn poll_and_deliver_external_interrupt_uses_interrupt_controller() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x21, 0x08, 0x6666, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x3000);
    cpu.state.set_rip(0x1111);
    cpu.state.set_rflags(0x202);

    let mut ctrl = OneShotController(Some(0x21));
    cpu.poll_and_deliver_external_interrupt(&mut mem, &mut ctrl)?;

    assert_eq!(cpu.state.rip(), 0x6666);
    Ok(())
}

struct CountingController {
    vector: u8,
    poll_count: usize,
}

impl CountingController {
    fn new(vector: u8) -> Self {
        Self {
            vector,
            poll_count: 0,
        }
    }
}

impl InterruptController for CountingController {
    fn poll_interrupt(&mut self) -> Option<u8> {
        self.poll_count += 1;
        Some(self.vector)
    }
}

#[test]
fn poll_and_deliver_external_interrupt_does_not_poll_controller_when_if0() -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);
    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.state.set_rflags(0); // IF=0

    let mut ctrl = CountingController::new(0x20);
    cpu.poll_and_deliver_external_interrupt(&mut mem, &mut ctrl)?;

    assert_eq!(ctrl.poll_count, 0);
    assert!(cpu.pending.external_interrupts.is_empty());
    Ok(())
}

#[test]
fn poll_and_deliver_external_interrupt_does_not_poll_controller_when_interrupt_shadow_active(
) -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);
    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.state.set_rflags(RFLAGS_IF);
    cpu.pending.inhibit_interrupts_for_one_instruction();

    let mut ctrl = CountingController::new(0x20);
    cpu.poll_and_deliver_external_interrupt(&mut mem, &mut ctrl)?;

    assert_eq!(ctrl.poll_count, 0);
    assert!(cpu.pending.external_interrupts.is_empty());
    Ok(())
}

#[test]
fn poll_and_deliver_external_interrupt_delivers_queued_vector_before_polling_controller(
) -> Result<(), CpuExit> {
    let mut mem = FlatTestBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x21, 0x08, 0x6666, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x3000);
    cpu.state.set_rip(0x1111);
    cpu.state.set_rflags(0x202);

    // Already-queued vector should be delivered without polling/acknowledging any new interrupt.
    cpu.pending.inject_external_interrupt(0x21);

    let mut ctrl = CountingController::new(0x22);
    cpu.poll_and_deliver_external_interrupt(&mut mem, &mut ctrl)?;

    assert_eq!(ctrl.poll_count, 0);
    assert!(cpu.pending.external_interrupts.is_empty());
    assert_eq!(cpu.state.rip(), 0x6666);
    Ok(())
}
