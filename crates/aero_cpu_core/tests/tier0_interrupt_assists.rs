use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, RFLAGS_IF, RFLAGS_RESERVED1};
use aero_cpu_core::CpuBus;
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

fn write_idt_gate32(bus: &mut FlatTestBus, base: u64, vector: u8, selector: u16, offset: u32, type_attr: u8) {
    let addr = base + (vector as u64) * 8;
    bus.write_u16(addr, (offset & 0xFFFF) as u16).unwrap();
    bus.write_u16(addr + 2, selector).unwrap();
    bus.write_u8(addr + 4, 0).unwrap();
    bus.write_u8(addr + 5, type_attr).unwrap();
    bus.write_u16(addr + 6, (offset >> 16) as u16).unwrap();
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
    bus.write_u64(GDT_BASE + 0x00, 0).unwrap();
    bus.write_u64(GDT_BASE + 0x08, seg_desc(0, 0xFFFFF, 0xA, 0)).unwrap();
    bus.write_u64(GDT_BASE + 0x10, seg_desc(0, 0xFFFFF, 0x2, 0)).unwrap();

    // IDT[0x80] -> HANDLER_BASE (interrupt gate, DPL0).
    write_idt_gate32(&mut bus, IDT_BASE, 0x80, 0x08, HANDLER_BASE as u32, 0x8E);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;
    state.set_rip(CODE_BASE);
    state.segments.cs.selector = 0x08;
    state.segments.ss.selector = 0x10;
    state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);
    state.tables.gdtr.base = GDT_BASE;
    state.tables.gdtr.limit = 0x18 - 1;
    state.tables.idtr.base = IDT_BASE;
    state.tables.idtr.limit = (0x80 * 8 + 7) as u16;

    // Push sentinel return address.
    let sp_pushed = STACK_TOP - 4;
    bus.write_u32(sp_pushed, RETURN_IP).unwrap();
    state.write_reg(Register::ESP, sp_pushed);

    let mut ctx = AssistContext::default();
    let mut executed = 0u64;
    loop {
        if state.rip() == RETURN_IP as u64 {
            break;
        }
        let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 1024);
        executed += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => panic!("unexpected HLT at rip=0x{:X}", state.rip()),
            BatchExit::BiosInterrupt(vector) => {
                panic!("unexpected BIOS interrupt {vector:#x} at rip=0x{:X}", state.rip())
            }
            BatchExit::Assist(r) => panic!("unexpected unhandled assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception after {executed} insts: {e:?}"),
        }
    }

    assert_eq!(bus.read_u32(0x500).unwrap(), 0xCAFE_BABE);
    assert!(state.get_flag(RFLAGS_IF));
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
    bus.write_u64(GDT_BASE + 0x00, 0).unwrap();
    // 0x08 ring0 code (exec+read)
    bus.write_u64(GDT_BASE + 0x08, seg_desc(0, 0xFFFFF, 0xA, 0)).unwrap();
    // 0x10 ring0 data (rw)
    bus.write_u64(GDT_BASE + 0x10, seg_desc(0, 0xFFFFF, 0x2, 0)).unwrap();
    // 0x18 ring3 code
    bus.write_u64(GDT_BASE + 0x18, seg_desc(0, 0xFFFFF, 0xA, 3)).unwrap();
    // 0x20 ring3 data
    bus.write_u64(GDT_BASE + 0x20, seg_desc(0, 0xFFFFF, 0x2, 3)).unwrap();

    // TSS32 ring0 stack (SS0:ESP0).
    bus.write_u32(TSS_BASE + 4, KERNEL_STACK_TOP as u32).unwrap();
    bus.write_u16(TSS_BASE + 8, 0x10).unwrap();

    // IDT[0x80] -> HANDLER_BASE (interrupt gate, DPL3 so CPL3 can invoke it).
    write_idt_gate32(&mut bus, IDT_BASE, 0x80, 0x08, HANDLER_BASE as u32, 0xEE);

    // User code: int 0x80; mov [0x500], eax; ret
    let code: [u8; 8] = [0xCD, 0x80, 0xA3, 0x00, 0x05, 0x00, 0x00, 0xC3];
    bus.load(CODE_BASE, &code);

    // Kernel handler: mov eax, 0x1337; iretd
    let handler: [u8; 6] = [0xB8, 0x37, 0x13, 0x00, 0x00, 0xCF];
    bus.load(HANDLER_BASE, &handler);

    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;
    state.set_rip(CODE_BASE);
    state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);
    state.segments.cs.selector = 0x1B; // user code selector (0x18 | RPL3)
    state.segments.ss.selector = 0x23; // user data selector (0x20 | RPL3)
    state.tables.gdtr.base = GDT_BASE;
    state.tables.gdtr.limit = 0x28 - 1; // 5 entries * 8 - 1
    state.tables.idtr.base = IDT_BASE;
    state.tables.idtr.limit = (0x80 * 8 + 7) as u16;
    // Mark TR as usable and point it at the in-memory TSS.
    state.tables.tr.base = TSS_BASE;
    state.tables.tr.limit = 0x67;
    state.tables.tr.access = 0;

    // Push sentinel return address on the user stack.
    let sp_pushed = USER_STACK_TOP - 4;
    bus.write_u32(sp_pushed, RETURN_IP).unwrap();
    state.write_reg(Register::ESP, sp_pushed);

    let mut ctx = AssistContext::default();
    let mut executed = 0u64;
    loop {
        if state.rip() == RETURN_IP as u64 {
            break;
        }
        let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 1024);
        executed += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => panic!("unexpected HLT at rip=0x{:X}", state.rip()),
            BatchExit::BiosInterrupt(vector) => {
                panic!("unexpected BIOS interrupt {vector:#x} at rip=0x{:X}", state.rip())
            }
            BatchExit::Assist(r) => panic!("unexpected unhandled assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception after {executed} insts: {e:?}"),
        }
    }

    assert_eq!(bus.read_u32(0x500).unwrap(), 0x1337);
    assert_eq!(state.segments.cs.selector, 0x1B);
    assert_eq!(state.segments.ss.selector, 0x23);
    assert!(state.get_flag(RFLAGS_IF));

    // Kernel stack frame should have been constructed at KERNEL_STACK_TOP - 20.
    let frame_base = KERNEL_STACK_TOP - 20;
    assert_eq!(bus.read_u32(frame_base).unwrap(), (CODE_BASE + 2) as u32); // return EIP
    assert_eq!(bus.read_u32(frame_base + 4).unwrap() as u16, 0x1B); // old CS
    assert_eq!(bus.read_u32(frame_base + 8).unwrap() & 0xFFFF_FFFF, (RFLAGS_RESERVED1 | RFLAGS_IF) as u32); // old EFLAGS
    assert_eq!(bus.read_u32(frame_base + 12).unwrap(), sp_pushed as u32); // old ESP
    assert_eq!(bus.read_u32(frame_base + 16).unwrap() as u16, 0x23); // old SS
}
