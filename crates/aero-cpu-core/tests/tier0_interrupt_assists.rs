use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CR0_PE, RFLAGS_IF, RFLAGS_RESERVED1, SEG_ACCESS_PRESENT};
use aero_cpu_core::CpuBus;
use aero_cpu_core::CpuCore;
use aero_x86::Register;

fn seg_desc(base: u32, limit: u32, typ: u8, dpl: u8) -> u64 {
    // 8-byte segment descriptor (legacy 32-bit format).
    let limit_low = (limit & 0xFFFF) as u64;
    let base_low = (base & 0xFFFF) as u64;
    let base_mid = ((base >> 16) & 0xFF) as u64;
    let base_high = ((base >> 24) & 0xFF) as u64;
    let limit_high = ((limit >> 16) & 0xF) as u64;

    let s = 1u64; // code/data
    let present = 1u64;
    let db = 1u64; // 32-bit
    let g = 1u64; // 4K granularity

    let access = (typ as u64 & 0xF) | (s << 4) | ((dpl as u64 & 0x3) << 5) | (present << 7);
    let flags = (db << 2) | (g << 3);

    limit_low
        | (base_low << 16)
        | (base_mid << 32)
        | (access << 40)
        | (limit_high << 48)
        | (flags << 52)
        | (base_high << 56)
}

fn write_idt_gate32(
    bus: &mut FlatTestBus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 8;
    bus.write_u16(addr, (offset & 0xFFFF) as u16).unwrap();
    bus.write_u16(addr + 2, selector).unwrap();
    bus.write_u8(addr + 4, 0).unwrap();
    bus.write_u8(addr + 5, type_attr).unwrap();
    bus.write_u16(addr + 6, (offset >> 16) as u16).unwrap();
}

fn write_idt_gate64(
    bus: &mut FlatTestBus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u64,
    ist: u8,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 16;
    bus.write_u16(addr, (offset & 0xFFFF) as u16).unwrap();
    bus.write_u16(addr + 2, selector).unwrap();
    bus.write_u8(addr + 4, ist & 0x7).unwrap();
    bus.write_u8(addr + 5, type_attr).unwrap();
    bus.write_u16(addr + 6, ((offset >> 16) & 0xFFFF) as u16)
        .unwrap();
    bus.write_u32(addr + 8, ((offset >> 32) & 0xFFFF_FFFF) as u32)
        .unwrap();
    bus.write_u32(addr + 12, 0).unwrap();
}

fn write_ivt_entry(bus: &mut FlatTestBus, vector: u8, offset: u16, segment: u16) {
    let addr = (vector as u64) * 4;
    bus.write_u16(addr, offset).unwrap();
    bus.write_u16(addr + 2, segment).unwrap();
}

#[test]
fn tier0_assists_protected_int_iret_no_privilege_change() {
    const BUS_SIZE: usize = 0x20000;
    const CODE_BASE: u64 = 0x1000;
    const HANDLER_BASE: u64 = 0x2000;
    const GDT_BASE: u64 = 0x2800;
    const IDT_BASE: u64 = 0x3000;
    const STACK_TOP: u64 = 0x9000;
    const RETURN_IP: u32 = 0xDEAD_BEEF;

    let mut bus = FlatTestBus::new(BUS_SIZE);

    // int 0x80; mov [0x500], eax; ret
    let code: [u8; 8] = [0xCD, 0x80, 0xA3, 0x00, 0x05, 0x00, 0x00, 0xC3];
    bus.load(CODE_BASE, &code);

    // mov eax, 0xCAFEBABE; iretd
    let handler: [u8; 6] = [0xB8, 0xBE, 0xBA, 0xFE, 0xCA, 0xCF];
    bus.load(HANDLER_BASE, &handler);

    // Minimal GDT: null + ring0 code + ring0 data.
    bus.write_u64(GDT_BASE, 0).unwrap();
    bus.write_u64(GDT_BASE + 0x08, seg_desc(0, 0xFFFFF, 0xA, 0))
        .unwrap();
    bus.write_u64(GDT_BASE + 0x10, seg_desc(0, 0xFFFFF, 0x2, 0))
        .unwrap();

    // IDT[0x80] -> HANDLER_BASE (interrupt gate, DPL0).
    write_idt_gate32(&mut bus, IDT_BASE, 0x80, 0x08, HANDLER_BASE as u32, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Bit32);
    cpu.state.control.cr0 |= CR0_PE;
    cpu.state.set_rip(CODE_BASE);
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);
    cpu.state.tables.gdtr.base = GDT_BASE;
    cpu.state.tables.gdtr.limit = 0x18 - 1;
    cpu.state.tables.idtr.base = IDT_BASE;
    cpu.state.tables.idtr.limit = (0x80 * 8 + 7) as u16;

    // Push sentinel return address.
    let sp_pushed = STACK_TOP - 4;
    bus.write_u32(sp_pushed, RETURN_IP).unwrap();
    cpu.state.write_reg(Register::ESP, sp_pushed);

    let mut ctx = AssistContext::default();
    let cfg = Tier0Config::default();
    let mut executed = 0u64;
    loop {
        if cpu.state.rip() == RETURN_IP as u64 {
            break;
        }
        let res = run_batch_cpu_core_with_assists(&cfg, &mut ctx, &mut cpu, &mut bus, 1024);
        executed += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => panic!("unexpected HLT at rip=0x{:X}", cpu.state.rip()),
            BatchExit::BiosInterrupt(vector) => {
                panic!(
                    "unexpected BIOS interrupt {vector:#x} at rip=0x{:X}",
                    cpu.state.rip()
                )
            }
            BatchExit::Assist(r) => panic!("unexpected unhandled assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception after {executed} insts: {e:?}"),
            BatchExit::CpuExit(e) => panic!("unexpected cpu exit after {executed} insts: {e:?}"),
        }
    }

    assert_eq!(bus.read_u32(0x500).unwrap(), 0xCAFE_BABE);
    assert!(cpu.state.get_flag(RFLAGS_IF));
}

#[test]
fn tier0_assists_protected_int_iret_switches_to_tss_stack() {
    const BUS_SIZE: usize = 0x30000;
    const CODE_BASE: u64 = 0x1000;
    const HANDLER_BASE: u64 = 0x2000;
    const GDT_BASE: u64 = 0x4000;
    const IDT_BASE: u64 = 0x5000;
    const TSS_BASE: u64 = 0x6000;
    const USER_STACK_TOP: u64 = 0x9000;
    const KERNEL_STACK_TOP: u64 = 0xA000;
    const RETURN_IP: u32 = 0xDEAD_BEEF;

    let mut bus = FlatTestBus::new(BUS_SIZE);

    // Build a small GDT with flat ring0/ring3 segments.
    // 0x00 null
    bus.write_u64(GDT_BASE, 0).unwrap();
    // 0x08 ring0 code (exec+read)
    bus.write_u64(GDT_BASE + 0x08, seg_desc(0, 0xFFFFF, 0xA, 0))
        .unwrap();
    // 0x10 ring0 data (rw)
    bus.write_u64(GDT_BASE + 0x10, seg_desc(0, 0xFFFFF, 0x2, 0))
        .unwrap();
    // 0x18 ring3 code
    bus.write_u64(GDT_BASE + 0x18, seg_desc(0, 0xFFFFF, 0xA, 3))
        .unwrap();
    // 0x20 ring3 data
    bus.write_u64(GDT_BASE + 0x20, seg_desc(0, 0xFFFFF, 0x2, 3))
        .unwrap();

    // TSS32 ring0 stack (SS0:ESP0).
    bus.write_u32(TSS_BASE + 4, KERNEL_STACK_TOP as u32)
        .unwrap();
    bus.write_u16(TSS_BASE + 8, 0x10).unwrap();

    // IDT[0x80] -> HANDLER_BASE (interrupt gate, DPL3 so CPL3 can invoke it).
    write_idt_gate32(&mut bus, IDT_BASE, 0x80, 0x08, HANDLER_BASE as u32, 0xEE);

    // User code: int 0x80; mov [0x500], eax; ret
    let code: [u8; 8] = [0xCD, 0x80, 0xA3, 0x00, 0x05, 0x00, 0x00, 0xC3];
    bus.load(CODE_BASE, &code);

    // Kernel handler: mov eax, 0x1337; iretd
    let handler: [u8; 6] = [0xB8, 0x37, 0x13, 0x00, 0x00, 0xCF];
    bus.load(HANDLER_BASE, &handler);

    let mut cpu = CpuCore::new(CpuMode::Bit32);
    cpu.state.control.cr0 |= CR0_PE;
    cpu.state.set_rip(CODE_BASE);
    cpu.state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);
    cpu.state.segments.cs.selector = 0x1B; // user code selector (0x18 | RPL3)
    cpu.state.segments.ss.selector = 0x23; // user data selector (0x20 | RPL3)
    cpu.state.tables.gdtr.base = GDT_BASE;
    cpu.state.tables.gdtr.limit = 0x28 - 1; // 5 entries * 8 - 1
    cpu.state.tables.idtr.base = IDT_BASE;
    cpu.state.tables.idtr.limit = (0x80 * 8 + 7) as u16;
    // Mark TR as usable and point it at the in-memory TSS.
    cpu.state.tables.tr.selector = 0x28;
    cpu.state.tables.tr.base = TSS_BASE;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    // Push sentinel return address on the user stack.
    let sp_pushed = USER_STACK_TOP - 4;
    bus.write_u32(sp_pushed, RETURN_IP).unwrap();
    cpu.state.write_reg(Register::ESP, sp_pushed);

    let mut ctx = AssistContext::default();
    let cfg = Tier0Config::default();
    let mut executed = 0u64;
    loop {
        if cpu.state.rip() == RETURN_IP as u64 {
            break;
        }
        let res = run_batch_cpu_core_with_assists(&cfg, &mut ctx, &mut cpu, &mut bus, 1024);
        executed += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => panic!("unexpected HLT at rip=0x{:X}", cpu.state.rip()),
            BatchExit::BiosInterrupt(vector) => {
                panic!(
                    "unexpected BIOS interrupt {vector:#x} at rip=0x{:X}",
                    cpu.state.rip()
                )
            }
            BatchExit::Assist(r) => panic!("unexpected unhandled assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception after {executed} insts: {e:?}"),
            BatchExit::CpuExit(e) => panic!("unexpected cpu exit after {executed} insts: {e:?}"),
        }
    }

    assert_eq!(bus.read_u32(0x500).unwrap(), 0x1337);
    assert_eq!(cpu.state.segments.cs.selector, 0x1B);
    assert_eq!(cpu.state.segments.ss.selector, 0x23);
    assert!(cpu.state.get_flag(RFLAGS_IF));

    // Kernel stack frame should have been constructed at KERNEL_STACK_TOP - 20.
    let frame_base = KERNEL_STACK_TOP - 20;
    assert_eq!(bus.read_u32(frame_base).unwrap(), (CODE_BASE + 2) as u32); // return EIP
    assert_eq!(bus.read_u32(frame_base + 4).unwrap() as u16, 0x1B); // old CS
    assert_eq!(
        bus.read_u32(frame_base + 8).unwrap(),
        (RFLAGS_RESERVED1 | RFLAGS_IF) as u32,
    ); // old EFLAGS
    assert_eq!(bus.read_u32(frame_base + 12).unwrap(), sp_pushed as u32); // old ESP
    assert_eq!(bus.read_u32(frame_base + 16).unwrap() as u16, 0x23); // old SS
}

#[test]
fn tier0_cpu_core_runner_executes_int_iretq_in_long_mode() {
    const BUS_SIZE: usize = 0x40000;
    const CODE_BASE: u64 = 0x1000;
    const HANDLER_BASE: u64 = 0x2000;
    const IDT_BASE: u64 = 0x3000;
    const INITIAL_RSP: u64 = 0x9000;

    let mut bus = FlatTestBus::new(BUS_SIZE);

    // Code: int 0x80; hlt
    bus.load(CODE_BASE, &[0xCD, 0x80, 0xF4]);

    // Handler: mov rax, 0x1234; iretq
    let handler: [u8; 12] = [
        0x48, 0xB8, 0x34, 0x12, 0, 0, 0, 0, 0, 0, // mov rax, 0x1234
        0x48, 0xCF, // iretq
    ];
    bus.load(HANDLER_BASE, &handler);

    write_idt_gate64(&mut bus, IDT_BASE, 0x80, 0x08, HANDLER_BASE, 0, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Bit64);
    cpu.state.tables.idtr.base = IDT_BASE;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_reg(Register::RSP, INITIAL_RSP);
    cpu.state.set_rflags(0x202); // IF=1
    cpu.state.set_rip(CODE_BASE);

    let mut ctx = AssistContext::default();
    let cfg = Tier0Config::default();
    let mut executed = 0u64;
    loop {
        if cpu.state.halted {
            break;
        }
        let res = run_batch_cpu_core_with_assists(&cfg, &mut ctx, &mut cpu, &mut bus, 1024);
        executed += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => break,
            BatchExit::BiosInterrupt(vector) => {
                panic!(
                    "unexpected BIOS interrupt {vector:#x} at rip=0x{:X}",
                    cpu.state.rip()
                )
            }
            BatchExit::Assist(r) => panic!("unexpected unhandled assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception after {executed} insts: {e:?}"),
            BatchExit::CpuExit(e) => panic!("unexpected cpu exit after {executed} insts: {e:?}"),
        }
    }

    assert!(cpu.state.halted);
    assert_eq!(cpu.state.read_reg(Register::RAX), 0x1234);
    assert_eq!(cpu.state.read_reg(Register::RSP), INITIAL_RSP);
    assert!(cpu.state.get_flag(RFLAGS_IF));
}

#[test]
fn tier0_cpu_core_runner_delivers_external_interrupt_after_sti_shadow() {
    const BUS_SIZE: usize = 0x10000;
    const CODE_BASE: u64 = 0x0100;
    const HANDLER_BASE: u64 = 0x0500;
    const STACK_TOP: u64 = 0x8000;

    let mut bus = FlatTestBus::new(BUS_SIZE);

    // Code: sti; nop; hlt
    bus.load(CODE_BASE, &[0xFB, 0x90, 0xF4]);
    // Handler: BIOS ROM stub `HLT; IRET`
    bus.load(HANDLER_BASE, &[0xF4, 0xCF]);
    // IVT[0x20] -> 0000:0500
    write_ivt_entry(&mut bus, 0x20, HANDLER_BASE as u16, 0);

    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.state.write_reg(Register::CS, 0);
    cpu.state.write_reg(Register::SS, 0);
    cpu.state.write_reg(Register::SP, STACK_TOP);
    cpu.state.set_rflags(RFLAGS_RESERVED1); // IF=0
    cpu.state.set_rip(CODE_BASE);
    cpu.pending.inject_external_interrupt(0x20);

    let mut ctx = AssistContext::default();
    let cfg = Tier0Config::default();
    let res = run_batch_cpu_core_with_assists(&cfg, &mut ctx, &mut cpu, &mut bus, 1024);

    // In real/v8086 mode the emulator records externally delivered vectors as pending BIOS
    // interrupts so the firmware's default IVT stubs (`HLT; IRET`) can be surfaced as BIOS
    // hypercalls rather than deadlocking (IF is cleared on interrupt entry).
    assert_eq!(res.exit, BatchExit::BiosInterrupt(0x20));
    assert_eq!(res.executed, 3);
    assert_eq!(cpu.state.rip(), HANDLER_BASE + 1);

    // Interrupt frame should return to the HLT instruction at CODE_BASE+2.
    let sp = cpu.state.stack_ptr();
    assert_eq!(bus.read_u16(sp).unwrap(), (CODE_BASE + 2) as u16);
}

#[test]
fn tier0_cpu_core_runner_delivers_external_interrupt_after_mov_ss_shadow() {
    const BUS_SIZE: usize = 0x10000;
    const CODE_BASE: u64 = 0x0100;
    const HANDLER_BASE: u64 = 0x0500;
    const STACK_TOP: u64 = 0x8000;

    let mut bus = FlatTestBus::new(BUS_SIZE);

    // Code: mov ss, ax; nop; hlt
    bus.load(CODE_BASE, &[0x8E, 0xD0, 0x90, 0xF4]);
    // Handler: BIOS ROM stub `HLT; IRET`
    bus.load(HANDLER_BASE, &[0xF4, 0xCF]);
    // IVT[0x20] -> 0000:0500
    write_ivt_entry(&mut bus, 0x20, HANDLER_BASE as u16, 0);

    let mut cpu = CpuCore::new(CpuMode::Real);
    cpu.state.write_reg(Register::CS, 0);
    cpu.state.write_reg(Register::SS, 0);
    cpu.state.write_reg(Register::AX, 0);
    cpu.state.write_reg(Register::SP, STACK_TOP);
    cpu.state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);
    cpu.state.set_rip(CODE_BASE);

    let mut ctx = AssistContext::default();
    let cfg = Tier0Config::default();

    // Execute MOV SS (creates an interrupt shadow for the next instruction).
    let res = run_batch_cpu_core_with_assists(&cfg, &mut ctx, &mut cpu, &mut bus, 1);
    assert_eq!(res.exit, BatchExit::Completed);
    assert_eq!(res.executed, 1);
    assert_eq!(cpu.state.rip(), CODE_BASE + 2);

    cpu.pending.inject_external_interrupt(0x20);

    // NOP executes, then the interrupt is delivered before the final HLT.
    let res = run_batch_cpu_core_with_assists(&cfg, &mut ctx, &mut cpu, &mut bus, 1024);
    assert_eq!(res.exit, BatchExit::BiosInterrupt(0x20));
    assert_eq!(res.executed, 2);
    assert_eq!(cpu.state.rip(), HANDLER_BASE + 1);

    // Interrupt frame should return to the HLT instruction at CODE_BASE+3.
    let sp = cpu.state.stack_ptr();
    assert_eq!(bus.read_u16(sp).unwrap(), (CODE_BASE + 3) as u16);
}
