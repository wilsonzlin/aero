use aero_cpu_core::exec::{ExecCpu, Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode, RFLAGS_IF, SEG_ACCESS_PRESENT};
use aero_x86::Register;

#[allow(clippy::too_many_arguments)]
fn make_descriptor(
    base: u32,
    limit_raw: u32,
    typ: u8,
    s: bool,
    dpl: u8,
    present: bool,
    avl: bool,
    l: bool,
    db: bool,
    g: bool,
) -> u64 {
    let mut raw = 0u64;
    raw |= (limit_raw & 0xFFFF) as u64;
    raw |= ((base & 0xFFFF) as u64) << 16;
    raw |= (((base >> 16) & 0xFF) as u64) << 32;
    let access =
        (typ as u64) | ((s as u64) << 4) | (((dpl as u64) & 0x3) << 5) | ((present as u64) << 7);
    raw |= access << 40;
    raw |= (((limit_raw >> 16) & 0xF) as u64) << 48;
    let flags = (avl as u64) | ((l as u64) << 1) | ((db as u64) << 2) | ((g as u64) << 3);
    raw |= flags << 52;
    raw |= (((base >> 24) & 0xFF) as u64) << 56;
    raw
}

fn setup_gdt(bus: &mut impl CpuBus, gdt_base: u64, descriptors: &[u64]) {
    for (i, &desc) in descriptors.iter().enumerate() {
        bus.write_u64(gdt_base + (i as u64) * 8, desc).unwrap();
    }
}

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

fn run_to_halt<B: CpuBus>(cpu: &mut Vcpu<B>, interp: &mut Tier0Interpreter, max_iters: u64) {
    for _ in 0..max_iters {
        if cpu.exit.is_some() {
            panic!("unexpected CPU exit: {:?}", cpu.exit);
        }
        if cpu.cpu.state.halted {
            return;
        }
        interp.exec_block(cpu);
    }
    panic!("program did not halt");
}

#[test]
fn tier0_executes_cpuid_assist_in_exec_glue() {
    let mut bus = FlatTestBus::new(0x1000);
    let code_base = 0x0000u64;
    bus.load(code_base, &[0x0F, 0xA2, 0xF4]); // CPUID; HLT

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x800);
    cpu.cpu.state.write_reg(Register::EAX, 0);
    cpu.cpu.state.write_reg(Register::ECX, 0);
    cpu.cpu.state.set_rflags(0x0002);
    cpu.cpu.state.set_rip(code_base);

    let mut interp = Tier0Interpreter::new(1024);
    run_to_halt(&mut cpu, &mut interp, 16);

    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.read_reg(Register::EAX), 0x1F);
    assert_eq!(
        cpu.cpu.state.read_reg(Register::EBX),
        u64::from(u32::from_le_bytes(*b"Genu"))
    );
    assert_eq!(
        cpu.cpu.state.read_reg(Register::EDX),
        u64::from(u32::from_le_bytes(*b"ineI"))
    );
    assert_eq!(
        cpu.cpu.state.read_reg(Register::ECX),
        u64::from(u32::from_le_bytes(*b"ntel"))
    );
}

#[test]
fn tier0_mov_ss_inhibits_external_interrupt_for_one_instruction() {
    let mut bus = FlatTestBus::new(0x20000);

    let gdt_base = 0x0800u64;
    let idt_base = 0x1000u64;
    let code_base = 0x2000u64;
    let handler = 0x3000u32;
    let data_addr = 0x4000u32;

    // Minimal flat 32-bit GDT: null, code, data.
    let null = 0u64;
    let code32 = make_descriptor(0, 0xFFFFF, 0xA, true, 0, true, false, false, true, true);
    let data32 = make_descriptor(0, 0xFFFFF, 0x2, true, 0, true, false, false, true, true);
    setup_gdt(&mut bus, gdt_base, &[null, code32, data32]);

    // Interrupt handler: HLT
    bus.load(handler as u64, &[0xF4]);
    write_idt_gate32(&mut bus, idt_base, 0x20, 0x08, handler, 0x8E);

    // Code:
    //   mov ss, ax
    //   mov byte ptr [data_addr], 0xAA
    //   hlt (should not run if interrupt is delivered after the store)
    let mut code = vec![
        0x8E, 0xD0, // mov ss, ax
        0xC6, 0x05, // mov byte ptr [disp32], imm8
    ];
    code.extend_from_slice(&data_addr.to_le_bytes());
    code.push(0xAA);
    code.push(0xF4);
    bus.load(code_base, &code);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.tables.gdtr.base = gdt_base;
    cpu.cpu.state.tables.gdtr.limit = (3 * 8 - 1) as u16;
    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.segments.ds.selector = 0x10;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.segments.ds.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x9000);
    cpu.cpu.state.write_reg(Register::AX, 0x10);
    cpu.cpu.state.set_rflags(0x202);
    cpu.cpu.state.set_rip(code_base);
    cpu.cpu.state.set_protected_enable(true);

    let mut interp = Tier0Interpreter::new(1024);

    // Execute MOV SS. This is a privileged assist and should set the interrupt shadow.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), code_base + 2);
    assert_eq!(cpu.bus.read_u8(data_addr as u64).unwrap(), 0);

    // Make an external interrupt pending immediately after MOV SS.
    cpu.cpu.pending.inject_external_interrupt(0x20);

    // The following instruction (the store) must execute before the interrupt is delivered.
    run_to_halt(&mut cpu, &mut interp, 32);
    assert_eq!(cpu.bus.read_u8(data_addr as u64).unwrap(), 0xAA);
}

#[test]
fn tier0_cli_in_user_mode_raises_gp() {
    let mut bus = FlatTestBus::new(0x20000);

    let idt_base = 0x1000u64;
    let handler = 0x2000u32;
    let tss_base = 0x3000u64;

    // Code (CPL3): CLI; (should fault); HLT (should never execute)
    bus.load(0, &[0xFA, 0xF4]);

    // #GP handler: HLT (CPL0).
    bus.load(handler as u64, &[0xF4]);
    write_idt_gate32(&mut bus, idt_base, 13, 0x08, handler, 0x8E);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.cpu.state.segments.ss.selector = 0x23;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x7000);
    cpu.cpu.state.set_rflags(0x202); // IF=1, IOPL=0
    cpu.cpu.state.set_rip(0);

    // Provide a ring-0 stack in a 32-bit TSS so the exception can switch privilege levels.
    cpu.cpu.state.tables.tr.selector = 0x28;
    cpu.cpu.state.tables.tr.base = tss_base;
    cpu.cpu.state.tables.tr.limit = 0x67;
    cpu.cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    cpu.bus.write_u32(tss_base + 4, 0x9000).unwrap(); // ESP0
    cpu.bus.write_u16(tss_base + 8, 0x10).unwrap(); // SS0

    let mut interp = Tier0Interpreter::new(1024);

    // First block should deliver #GP and transfer to the handler.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), handler as u64);
    assert_eq!(cpu.cpu.state.segments.cs.selector, 0x08);

    run_to_halt(&mut cpu, &mut interp, 16);
    assert!(cpu.cpu.state.halted);
}

#[test]
fn tier0_hlt_in_user_mode_delivers_gp() {
    let mut bus = FlatTestBus::new(0x20000);

    let idt_base = 0x1000u64;
    let handler = 0x2000u32;
    let tss_base = 0x3000u64;

    // Code (CPL3): HLT; (should fault); HLT (should never execute).
    bus.load(0, &[0xF4, 0xF4]);

    // #GP handler: HLT (CPL0).
    bus.load(handler as u64, &[0xF4]);
    write_idt_gate32(&mut bus, idt_base, 13, 0x08, handler, 0x8E);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.cpu.state.segments.ss.selector = 0x23;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x7000);
    cpu.cpu.state.set_rflags(0x202); // IF=1, IOPL=0
    cpu.cpu.state.set_rip(0);

    // Provide a ring-0 stack in a 32-bit TSS so the exception can switch privilege levels.
    cpu.cpu.state.tables.tr.selector = 0x28;
    cpu.cpu.state.tables.tr.base = tss_base;
    cpu.cpu.state.tables.tr.limit = 0x67;
    cpu.cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    cpu.bus.write_u32(tss_base + 4, 0x9000).unwrap(); // ESP0
    cpu.bus.write_u16(tss_base + 8, 0x10).unwrap(); // SS0

    let mut interp = Tier0Interpreter::new(1024);

    // First block should deliver #GP and transfer to the handler.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), handler as u64);
    assert_eq!(cpu.cpu.state.segments.cs.selector, 0x08);

    run_to_halt(&mut cpu, &mut interp, 16);
    assert!(cpu.cpu.state.halted);
}

#[test]
fn tier0_assist_errors_deliver_gp() {
    let mut bus = FlatTestBus::new(0x20000);

    let idt_base = 0x1000u64;
    let handler = 0x2000u32;
    let tss_base = 0x3000u64;

    // Code (CPL3): mov cr3, eax; hlt (should fault before the HLT executes).
    bus.load(0, &[0x0F, 0x22, 0xD8, 0xF4]);

    // #GP handler: HLT (CPL0).
    bus.load(handler as u64, &[0xF4]);
    write_idt_gate32(&mut bus, idt_base, 13, 0x08, handler, 0x8E);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.cpu.state.segments.ss.selector = 0x23;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x7000);
    cpu.cpu.state.set_rflags(0x202);
    cpu.cpu.state.set_rip(0);

    // Provide a ring-0 stack in a 32-bit TSS so the exception can switch privilege levels.
    cpu.cpu.state.tables.tr.selector = 0x28;
    cpu.cpu.state.tables.tr.base = tss_base;
    cpu.cpu.state.tables.tr.limit = 0x67;
    cpu.cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    cpu.bus.write_u32(tss_base + 4, 0x9000).unwrap(); // ESP0
    cpu.bus.write_u16(tss_base + 8, 0x10).unwrap(); // SS0

    let mut interp = Tier0Interpreter::new(1024);

    // First block should deliver #GP and transfer to the handler.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), handler as u64);
    assert_eq!(cpu.cpu.state.segments.cs.selector, 0x08);

    run_to_halt(&mut cpu, &mut interp, 16);
    assert!(cpu.cpu.state.halted);
}

#[test]
fn tier0_preserves_bios_interrupt_vector_for_hlt_hypercall() {
    let mut bus = FlatTestBus::new(0x100000);

    // Real-mode code: `int 10h` to jump into the IVT handler.
    let code_base = 0x0100u64;
    bus.load(code_base, &[0xCD, 0x10]);

    // IVT entry for INT 10h points to a tiny ROM stub that begins with `HLT`.
    let vector = 0x10u8;
    let stub_seg = 0xF000u16;
    let stub_off = 0x0000u16;
    let ivt_addr = (vector as u64) * 4;
    bus.write_u16(ivt_addr, stub_off).unwrap();
    bus.write_u16(ivt_addr + 2, stub_seg).unwrap();

    // Stub: HLT; IRET. Tier-0 should surface the HLT as `BiosInterrupt(vector)`
    // and leave RIP pointing at the IRET so the embedding can resume after
    // dispatching the BIOS interrupt.
    let stub_phys = (stub_seg as u64) << 4;
    bus.load(stub_phys, &[0xF4, 0xCF]);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Real, bus);
    cpu.cpu.state.write_reg(Register::CS, 0);
    cpu.cpu.state.write_reg(Register::DS, 0);
    cpu.cpu.state.write_reg(Register::SS, 0);
    cpu.cpu.state.write_reg(Register::SP, 0x8000);
    cpu.cpu.state.set_rflags(0x0002);
    cpu.cpu.state.set_rip(code_base);

    let mut interp = Tier0Interpreter::new(1024);

    // First block executes the INT and stops at the handler entry (branch).
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.segments.cs.selector, stub_seg);
    assert_eq!(cpu.cpu.state.rip(), stub_off as u64);
    assert!(cpu.cpu.state.pending_bios_int_valid);
    assert_eq!(cpu.cpu.state.pending_bios_int, vector);

    // Second block executes the HLT and stops at the hypercall boundary.
    interp.exec_block(&mut cpu);
    assert!(!cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.rip(), (stub_off as u64) + 1);
    // `step()` consumes the BIOS vector when producing `StepExit::BiosInterrupt`;
    // the tier-0 exec glue should restore it so the embedding can observe it.
    assert!(cpu.cpu.state.pending_bios_int_valid);
    assert_eq!(cpu.cpu.state.pending_bios_int, vector);
}

#[test]
fn tier0_external_interrupt_to_bios_stub_exits_instead_of_halting() {
    let mut bus = FlatTestBus::new(0x100000);
    let code_base = 0x0100u64;
    bus.load(code_base, &[0x90]); // NOP

    // IVT[0x20] points to a tiny ROM stub that begins with `HLT; IRET`.
    let vector = 0x20u8;
    let stub_seg = 0xF000u16;
    let stub_off = 0x0000u16;
    let ivt_addr = (vector as u64) * 4;
    bus.write_u16(ivt_addr, stub_off).unwrap();
    bus.write_u16(ivt_addr + 2, stub_seg).unwrap();
    let stub_phys = (stub_seg as u64) << 4;
    bus.load(stub_phys, &[0xF4, 0xCF]);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Real, bus);
    cpu.cpu.state.write_reg(Register::CS, 0);
    cpu.cpu.state.write_reg(Register::SS, 0);
    cpu.cpu.state.write_reg(Register::SP, 0x8000);
    cpu.cpu.state.set_rflags(0x0202); // IF=1
    cpu.cpu.state.set_rip(code_base);

    // Inject an external interrupt; Tier-0 should deliver it into the stub and
    // treat the stub's HLT as a BIOS interrupt hypercall exit rather than
    // halting with IF=0.
    cpu.cpu.pending.inject_external_interrupt(vector);

    let mut interp = Tier0Interpreter::new(1024);
    interp.exec_block(&mut cpu);

    assert!(!cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.segments.cs.selector, stub_seg);
    assert_eq!(cpu.cpu.state.rip(), (stub_off as u64) + 1);
    assert!(cpu.cpu.state.pending_bios_int_valid);
    assert_eq!(cpu.cpu.state.pending_bios_int, vector);
}

#[test]
fn tier0_mov_ss_sets_interrupt_shadow_in_real_mode() {
    let mut bus = FlatTestBus::new(0x20000);

    let code_base = 0x0100u64;
    // mov ss, ax; nop
    bus.load(code_base, &[0x8E, 0xD0, 0x90]);

    // IVT[0x20] -> 0000:0500
    let vector = 0x20u8;
    let handler_off = 0x0500u16;
    let ivt_addr = (vector as u64) * 4;
    bus.write_u16(ivt_addr, handler_off).unwrap();
    bus.write_u16(ivt_addr + 2, 0).unwrap();
    bus.load(handler_off as u64, &[0xF4]); // handler: HLT

    let mut cpu = Vcpu::new_with_mode(CpuMode::Real, bus);
    cpu.cpu.state.write_reg(Register::CS, 0);
    cpu.cpu.state.write_reg(Register::SS, 0);
    cpu.cpu.state.write_reg(Register::SP, 0x8000);
    cpu.cpu.state.write_reg(Register::AX, 0x1000);
    cpu.cpu.state.set_rflags(0x0202); // IF=1
    cpu.cpu.state.set_rip(code_base);

    let mut interp = Tier0Interpreter::new(1);

    // Execute MOV SS, AX. The exec glue should apply the MOV SS interrupt shadow.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), code_base + 2);
    assert_eq!(cpu.cpu.state.read_reg(Register::SS), 0x1000);

    cpu.cpu.pending.inject_external_interrupt(vector);
    assert!(
        !cpu.maybe_deliver_interrupt(),
        "external interrupt should be blocked by MOV SS shadow"
    );
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 1);

    // Execute the following instruction; the interrupt should still be blocked.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), code_base + 3);
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 1);

    // Shadow should now be aged out; the external interrupt should be deliverable.
    assert!(cpu.maybe_deliver_interrupt());
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 0);
    assert_eq!(cpu.cpu.state.rip(), handler_off as u64);
    assert_eq!(cpu.cpu.state.read_reg(Register::SP) as u16, 0x7FFA);
}

#[test]
fn tier0_pop_ss_sets_interrupt_shadow_in_real_mode() {
    let mut bus = FlatTestBus::new(0x20000);

    let code_base = 0x0100u64;
    // pop ss; nop
    bus.load(code_base, &[0x17, 0x90]);

    // IVT[0x20] -> 0000:0500
    let vector = 0x20u8;
    let handler_off = 0x0500u16;
    let ivt_addr = (vector as u64) * 4;
    bus.write_u16(ivt_addr, handler_off).unwrap();
    bus.write_u16(ivt_addr + 2, 0).unwrap();
    bus.load(handler_off as u64, &[0xF4]); // handler: HLT

    let mut cpu = Vcpu::new_with_mode(CpuMode::Real, bus);
    cpu.cpu.state.write_reg(Register::CS, 0);
    cpu.cpu.state.write_reg(Register::SS, 0);
    cpu.cpu.state.write_reg(Register::SP, 0x8000);
    cpu.cpu.state.set_rflags(0x0202); // IF=1
    cpu.cpu.state.set_rip(code_base);

    // Stack top contains the new SS selector.
    cpu.bus.write_u16(0x8000, 0x1000).unwrap();

    let mut interp = Tier0Interpreter::new(1);

    // Execute POP SS. The exec glue should apply the POP SS interrupt shadow.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), code_base + 1);
    assert_eq!(cpu.cpu.state.read_reg(Register::SS), 0x1000);
    assert_eq!(cpu.cpu.state.read_reg(Register::SP) as u16, 0x8002);

    cpu.cpu.pending.inject_external_interrupt(vector);
    assert!(
        !cpu.maybe_deliver_interrupt(),
        "external interrupt should be blocked by POP SS shadow"
    );
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 1);

    // Execute the following instruction; the interrupt should still be blocked.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.cpu.state.rip(), code_base + 2);
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 1);

    // Shadow should now be aged out; the external interrupt should be deliverable.
    assert!(cpu.maybe_deliver_interrupt());
    assert_eq!(cpu.cpu.pending.external_interrupts.len(), 0);
    assert_eq!(cpu.cpu.state.rip(), handler_off as u64);
    assert_eq!(cpu.cpu.state.read_reg(Register::SP) as u16, 0x7FFC);
}

#[test]
fn tier0_executes_int_iretd_in_protected_mode() {
    let mut bus = FlatTestBus::new(0x20000);

    let code_base = 0x0000;
    let handler = 0x2000u32;
    let idt_base = 0x1000u64;

    // Code: int 0x80; hlt
    bus.load(code_base, &[0xCD, 0x80, 0xF4]);
    // Handler: mov eax, 0x42; iretd
    bus.load(handler as u64, &[0xB8, 0x42, 0x00, 0x00, 0x00, 0xCF]);

    write_idt_gate32(&mut bus, idt_base, 0x80, 0x08, handler, 0x8E);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x1000);
    cpu.cpu.state.set_rflags(0x202); // IF=1
    cpu.cpu.state.set_rip(code_base);

    let mut interp = Tier0Interpreter::new(1024);
    run_to_halt(&mut cpu, &mut interp, 64);

    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.read_reg(Register::EAX), 0x42);
    assert_eq!(cpu.cpu.state.read_gpr32(gpr::RSP), 0x1000);
    assert_ne!(cpu.cpu.state.rflags() & RFLAGS_IF, 0);
}

#[test]
fn tier0_executes_int_iretq_cpl3_to_cpl0_stack_switch() {
    let mut bus = FlatTestBus::new(0x40000);

    let code_base = 0x1000u64;
    let handler1 = 0x3000u64;
    let handler2 = 0x3100u64;
    let idt_base = 0x2000u64;

    // Code (CPL3): int 0x80; int 0x81
    //
    // We avoid using `HLT` in CPL3 (it would #GP(0)) by issuing a second
    // software interrupt that halts in CPL0 after proving the first handler
    // returns via IRETQ correctly.
    bus.load(code_base, &[0xCD, 0x80, 0xCD, 0x81]);

    // Handler 1 (CPL0): mov rax, 0x1234; iretq
    bus.load(
        handler1,
        &[
            0x48, 0xB8, 0x34, 0x12, 0, 0, 0, 0, 0, 0, // mov rax, 0x1234
            0x48, 0xCF, // iretq
        ],
    );

    // Handler 2 (CPL0): mov ebx, 0x5678; hlt
    bus.load(handler2, &[0xBB, 0x78, 0x56, 0x00, 0x00, 0xF4]);

    write_idt_gate64(&mut bus, idt_base, 0x80, 0x08, handler1, 0, 0xEE);
    write_idt_gate64(&mut bus, idt_base, 0x81, 0x08, handler2, 0, 0xEE);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Long, bus);
    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.cpu.state.segments.cs.selector = 0x33; // CPL3
    cpu.cpu.state.segments.ss.selector = 0x2B;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr64(gpr::RSP, 0x7000);
    cpu.cpu.state.set_rflags(0x202);
    cpu.cpu.state.set_rip(code_base);

    let tss_base = 0x10000u64;
    cpu.cpu.state.tables.tr.selector = 0x40;
    cpu.cpu.state.tables.tr.base = tss_base;
    cpu.cpu.state.tables.tr.limit = 0x67;
    cpu.cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;
    cpu.bus.write_u64(tss_base + 4, 0x9000).unwrap();

    let mut interp = Tier0Interpreter::new(1024);
    run_to_halt(&mut cpu, &mut interp, 128);

    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.read_reg(Register::RAX), 0x1234);
    assert_eq!(cpu.cpu.state.read_reg(Register::EBX), 0x5678);

    // The second interrupt (0x81) should have switched to RSP0 and pushed the
    // CPL3 return frame. In particular, it must capture the restored CPL3
    // RSP/SS (proving the first handler's IRETQ returned correctly).
    let frame_base = cpu.cpu.state.read_gpr64(gpr::RSP);
    assert_eq!(frame_base, 0x9000 - 40);
    assert_eq!(cpu.cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.cpu.state.segments.ss.selector, 0);
    assert_ne!(cpu.bus.read_u64(frame_base + 16).unwrap() & RFLAGS_IF, 0); // saved RFLAGS
    assert_eq!(cpu.bus.read_u64(frame_base + 24).unwrap(), 0x7000); // old RSP
    assert_eq!(cpu.bus.read_u64(frame_base + 32).unwrap(), 0x2B); // old SS
}
