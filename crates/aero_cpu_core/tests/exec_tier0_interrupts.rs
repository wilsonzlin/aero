use aero_cpu_core::exec::{Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode, RFLAGS_IF};
use aero_cpu_core::system::Tss64;
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

    cpu.cpu.pending.tss64 = Some(Tss64 {
        rsp0: 0x9000,
        ..Tss64::default()
    });

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
