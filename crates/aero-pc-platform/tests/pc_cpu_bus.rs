use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::interrupts::{CpuCore, CpuExit};
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{gpr, CpuMode, CpuState};
use aero_pc_platform::{PcCpuBus, PcPlatform};
use aero_platform::interrupts::InterruptInput;

#[test]
fn cpu_core_bus_routes_port_io_to_toggle_a20() {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    // mov ax, 0; mov ds, ax; mov al, 0x11; mov [0], al
    // mov ax, 0xffff; mov ds, ax; mov al, 0x22; mov [0x10], al   (A20 disabled => aliases to 0)
    // mov al, 0x02; out 0x92, al                                  (enable A20)
    // mov al, 0x33; mov [0x10], al                                (A20 enabled => 0x100000)
    // hlt
    let code = [
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xD8, // mov ds,ax
        0xB0, 0x11, // mov al,0x11
        0xA2, 0x00, 0x00, // mov [0],al
        0xB8, 0xFF, 0xFF, // mov ax,0xffff
        0x8E, 0xD8, // mov ds,ax
        0xB0, 0x22, // mov al,0x22
        0xA2, 0x10, 0x00, // mov [0x10],al
        0xB0, 0x02, // mov al,0x02
        0xE6, 0x92, // out 0x92,al
        0xB0, 0x33, // mov al,0x33
        0xA2, 0x10, 0x00, // mov [0x10],al
        0xF4, // hlt
    ];
    let code_base = 0x200u64;
    bus.platform.memory.write_physical(code_base, &code);

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.set_stack_ptr(0x1000);
    cpu.segments.cs.selector = 0;
    cpu.segments.cs.base = 0;
    cpu.set_rip(code_base);

    let mut ctx = AssistContext::default();
    let res = run_batch_with_assists(&mut ctx, &mut cpu, &mut bus, 1024);
    assert_eq!(res.exit, BatchExit::Halted);

    assert_eq!(bus.platform.memory.read_u8(0), 0x22);
    assert_eq!(bus.platform.memory.read_u8(0x1_00000), 0x33);
}

fn write_idt_gate32(
    mem: &mut impl CpuBus,
    idt_base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let entry_addr = idt_base + (vector as u64) * 8;
    mem.write_u16(entry_addr, (offset & 0xffff) as u16).unwrap();
    mem.write_u16(entry_addr + 2, selector).unwrap();
    mem.write_u8(entry_addr + 4, 0).unwrap();
    mem.write_u8(entry_addr + 5, type_attr).unwrap();
    mem.write_u16(entry_addr + 6, (offset >> 16) as u16)
        .unwrap();
}

#[test]
fn cpu_core_can_deliver_pic_interrupt_through_pc_platform_bus() -> Result<(), CpuExit> {
    let platform = PcPlatform::new(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);
    let mut ctrl = bus.interrupt_controller();

    {
        let mut ints = bus.platform.interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.raise_irq(InterruptInput::IsaIrq(1));
    }

    let mut cpu = CpuCore::new(CpuMode::Protected);
    let idt_base = 0x1000;
    let handler = 0x6000;
    write_idt_gate32(&mut bus, idt_base, 0x21, 0x08, handler, 0x8e);

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7ff;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.cs.base = 0;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.segments.ss.base = 0;
    cpu.state.write_gpr32(gpr::RSP, 0x9000);
    cpu.state.set_rip(0x1111);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    cpu.poll_and_deliver_external_interrupt(&mut bus, &mut ctrl)?;
    assert_eq!(cpu.state.rip(), handler as u64);

    Ok(())
}
