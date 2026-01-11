use aero_cpu_core::interrupts::{CpuCore, CpuExit, InterruptController};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode, RFLAGS_IF, SEG_ACCESS_PRESENT};
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
