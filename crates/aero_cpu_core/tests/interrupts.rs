use aero_cpu_core::exceptions::Exception;
use aero_cpu_core::interrupts::{CpuExit, InterruptController};
use aero_cpu_core::system::{Cpu, CpuMode, DescriptorTableRegister, Tss32, Tss64};
use aero_cpu_core::{Bus, RamBus};

fn write_idt_gate32(
    mem: &mut impl Bus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 8;
    mem.write_u16(addr, (offset & 0xFFFF) as u16);
    mem.write_u16(addr + 2, selector);
    mem.write_u8(addr + 4, 0);
    mem.write_u8(addr + 5, type_attr);
    mem.write_u16(addr + 6, (offset >> 16) as u16);
}

fn write_idt_gate64(
    mem: &mut impl Bus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u64,
    ist: u8,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 16;
    mem.write_u16(addr, (offset & 0xFFFF) as u16);
    mem.write_u16(addr + 2, selector);
    mem.write_u8(addr + 4, ist & 0x7);
    mem.write_u8(addr + 5, type_attr);
    mem.write_u16(addr + 6, ((offset >> 16) & 0xFFFF) as u16);
    mem.write_u32(addr + 8, ((offset >> 32) & 0xFFFF_FFFF) as u32);
    mem.write_u32(addr + 12, 0);
}

#[test]
fn int_real_mode_uses_ivt_and_pushes_frame() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x40000);

    // IVT[0x10] = 2222:1111
    mem.write_u16(0x10 * 4, 0x1111);
    mem.write_u16(0x10 * 4 + 2, 0x2222);

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Real;
    cpu.cs = 0x1234;
    cpu.ss = 0x2000;
    cpu.rsp = 0xFFFE;
    cpu.rflags = 0x202; // IF=1

    cpu.raise_software_interrupt(0x10, 0x5678);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.cs, 0x2222);
    assert_eq!(cpu.rip, 0x1111);
    assert_eq!(cpu.rsp as u16, 0xFFF8);
    assert_eq!(cpu.rflags & (1 << 9), 0); // IF cleared

    let stack_base = (0x2000u64) << 4;
    assert_eq!(mem.read_u16(stack_base + 0xFFF8), 0x5678); // IP
    assert_eq!(mem.read_u16(stack_base + 0xFFFA), 0x1234); // CS
    assert_eq!(mem.read_u16(stack_base + 0xFFFC), 0x0202); // FLAGS

    Ok(())
}

#[test]
fn int_protected_mode_no_privilege_change_pushes_eflags_cs_eip() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x10000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x80, 0x08, 0x2000, 0x8E); // present, DPL0, int gate

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x7FF,
    };
    cpu.cs = 0x08;
    cpu.ss = 0x10;
    cpu.rsp = 0x1000;
    cpu.rflags = 0x202;

    cpu.raise_software_interrupt(0x80, 0x1234);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.rip, 0x2000);
    assert_eq!(cpu.rsp, 0x0FF4);
    assert_eq!(cpu.rflags & (1 << 9), 0); // IF cleared by interrupt gate

    assert_eq!(mem.read_u32(0x0FF4), 0x1234); // EIP
    assert_eq!(mem.read_u32(0x0FF8), 0x08); // CS
    assert_eq!(mem.read_u32(0x0FFC), 0x202); // EFLAGS

    Ok(())
}

#[test]
fn int_protected_mode_cpl3_to_cpl0_stack_switch_and_iret_restore() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x80, 0x08, 0x3000, 0xEE); // present, DPL3, int gate

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x7FF,
    };
    cpu.cs = 0x1B; // CPL3
    cpu.ss = 0x23;
    cpu.rsp = 0x8000;
    cpu.rflags = 0x202;
    cpu.tss32 = Some(Tss32 {
        ss0: 0x10,
        esp0: 0x9000,
        ..Tss32::default()
    });

    cpu.raise_software_interrupt(0x80, 0x0040_0000);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.cs, 0x08);
    assert_eq!(cpu.ss, 0x10);
    assert_eq!(cpu.rip, 0x3000);
    assert_eq!(cpu.rsp, 0x8FEC);

    // New stack frame (top -> bottom): EIP, CS, EFLAGS, old ESP, old SS.
    assert_eq!(mem.read_u32(0x8FEC), 0x0040_0000);
    assert_eq!(mem.read_u32(0x8FF0), 0x1B);
    assert_eq!(mem.read_u32(0x8FF4), 0x202);
    assert_eq!(mem.read_u32(0x8FF8), 0x8000);
    assert_eq!(mem.read_u32(0x8FFC), 0x23);

    cpu.iret(&mut mem)?;

    assert_eq!(cpu.cs, 0x1B);
    assert_eq!(cpu.ss, 0x23);
    assert_eq!(cpu.rip, 0x0040_0000);
    assert_eq!(cpu.rsp, 0x8000);
    assert_ne!(cpu.rflags & (1 << 9), 0); // IF restored

    Ok(())
}

#[test]
fn page_fault_sets_cr2_and_pushes_error_code() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 14, 0x08, 0x4000, 0x8E);

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x7FF,
    };
    cpu.cs = 0x08;
    cpu.ss = 0x10;
    cpu.rsp = 0x2000;
    cpu.rflags = 0x202;

    cpu.raise_exception_fault(
        Exception::PageFault,
        0x1234_5678,
        Some(0xDEAD),
        Some(0xCAFE_BABE),
    );
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.cr2, 0xCAFE_BABE);
    assert_eq!(cpu.rip, 0x4000);
    assert_eq!(cpu.rsp, 0x1FF0);

    // top -> bottom: error_code, eip, cs, eflags
    assert_eq!(mem.read_u32(0x1FF0), 0xDEAD);
    assert_eq!(mem.read_u32(0x1FF4), 0x1234_5678);
    assert_eq!(mem.read_u32(0x1FF8), 0x08);
    assert_eq!(mem.read_u32(0x1FFC), 0x202);

    Ok(())
}

#[test]
fn sti_shadow_blocks_immediate_external_interrupt_delivery() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x20, 0x08, 0x5555, 0x8E);

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x7FF,
    };
    cpu.cs = 0x08;
    cpu.ss = 0x10;
    cpu.rsp = 0x3000;
    cpu.rip = 0x1111;
    cpu.cli().unwrap();
    cpu.sti().unwrap(); // sets IF=1 and creates an interrupt shadow

    cpu.inject_external_interrupt(0x20);
    cpu.deliver_external_interrupt(&mut mem)?;
    // Not delivered because of STI shadow.
    assert_eq!(cpu.rip, 0x1111);
    assert_eq!(cpu.external_interrupts.len(), 1);

    // Age the STI interrupt shadow.
    cpu.retire_instruction();

    cpu.deliver_external_interrupt(&mut mem)?;
    assert_eq!(cpu.rip, 0x5555);
    assert_eq!(cpu.external_interrupts.len(), 0);
    Ok(())
}

#[test]
fn mov_ss_shadow_blocks_immediate_external_interrupt_delivery() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x20, 0x08, 0x7777, 0x8E);

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x7FF,
    };
    cpu.cs = 0x08;
    cpu.ss = 0x10;
    cpu.rsp = 0x3000;
    cpu.rip = 0x1111;
    cpu.rflags = Cpu::RFLAGS_FIXED1 | Cpu::RFLAGS_IF;

    // Model MOV SS / POP SS interrupt shadow (does not touch IF).
    cpu.inhibit_interrupts_for_one_instruction();

    cpu.inject_external_interrupt(0x20);
    cpu.deliver_external_interrupt(&mut mem)?;
    assert_eq!(cpu.rip, 0x1111);
    assert_eq!(cpu.external_interrupts.len(), 1);

    cpu.retire_instruction();
    cpu.deliver_external_interrupt(&mut mem)?;
    assert_eq!(cpu.rip, 0x7777);
    assert_eq!(cpu.external_interrupts.len(), 0);

    Ok(())
}

#[test]
fn int_long_mode_cpl3_to_cpl0_uses_rsp0_and_iretq_returns() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x40000);

    let idt_base = 0x1000;
    write_idt_gate64(
        &mut mem, idt_base, 0x80, 0x08, 0x5000, 0, 0xEE, // present, DPL3, interrupt gate
    );

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Long64;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x0FFF,
    };
    cpu.cs = 0x33; // user code (CPL3)
    cpu.ss = 0x2B; // user data
    cpu.rip = 0x4000_0000;
    cpu.rsp = 0x7000;
    cpu.rflags = 0x202;
    cpu.tss64 = Some(Tss64 {
        rsp0: 0x9000,
        ..Tss64::default()
    });

    cpu.raise_software_interrupt(0x80, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    assert_eq!(cpu.cs, 0x08);
    assert_eq!(cpu.rip, 0x5000);
    assert_eq!(cpu.rsp, 0x9000 - 40);
    assert_eq!(cpu.rflags & (1 << 9), 0); // IF cleared on entry

    let frame_base = cpu.rsp;
    assert_eq!(mem.read_u64(frame_base), 0x4000_0010); // RIP
    assert_eq!(mem.read_u64(frame_base + 8), 0x33); // CS
    assert_eq!(mem.read_u64(frame_base + 16), 0x202); // RFLAGS
    assert_eq!(mem.read_u64(frame_base + 24), 0x7000); // RSP
    assert_eq!(mem.read_u64(frame_base + 32), 0x2B); // SS

    cpu.iret(&mut mem)?;

    assert_eq!(cpu.cs, 0x33);
    assert_eq!(cpu.ss, 0x2B);
    assert_eq!(cpu.rip, 0x4000_0010);
    assert_eq!(cpu.rsp, 0x7000);
    assert_ne!(cpu.rflags & (1 << 9), 0); // IF restored

    Ok(())
}

#[test]
fn int_long_mode_ist_overrides_rsp0() -> Result<(), CpuExit> {
    let mut mem = RamBus::new(0x40000);

    let idt_base = 0x1000;
    write_idt_gate64(&mut mem, idt_base, 0x81, 0x08, 0x6000, 1, 0xEE);

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Long64;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x0FFF,
    };
    cpu.cs = 0x33; // user code (CPL3)
    cpu.ss = 0x2B; // user data
    cpu.rip = 0x4000_0000;
    cpu.rsp = 0x7000;
    cpu.rflags = 0x202;
    cpu.tss64 = Some(Tss64 {
        rsp0: 0x9000,
        ist: [0xA000, 0, 0, 0, 0, 0, 0],
        ..Tss64::default()
    });

    cpu.raise_software_interrupt(0x81, 0x4000_0010);
    cpu.deliver_pending_event(&mut mem)?;

    // IST1 overrides RSP0.
    assert_eq!(cpu.rsp, 0xA000 - 40);

    cpu.iret(&mut mem)?;
    assert_eq!(cpu.rip, 0x4000_0010);
    assert_eq!(cpu.rsp, 0x7000);
    assert_eq!(cpu.ss, 0x2B);
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
    let mut mem = RamBus::new(0x20000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x21, 0x08, 0x6666, 0x8E);

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x7FF,
    };
    cpu.cs = 0x08;
    cpu.ss = 0x10;
    cpu.rsp = 0x3000;
    cpu.rip = 0x1111;
    cpu.rflags = 0x202;

    let mut ctrl = OneShotController(Some(0x21));
    cpu.poll_and_deliver_external_interrupt(&mut mem, &mut ctrl)?;

    assert_eq!(cpu.rip, 0x6666);
    Ok(())
}
