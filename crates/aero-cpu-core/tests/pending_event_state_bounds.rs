use aero_cpu_core::interrupts::{CpuCore, CpuExit, PendingEventState};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode};

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

#[test]
fn external_interrupt_queue_is_bounded_and_counts_drops() {
    let mut pending = PendingEventState::default();

    // Fill the queue to the hard cap.
    for _ in 0..PendingEventState::MAX_EXTERNAL_INTERRUPTS {
        pending.inject_external_interrupt(0x20);
    }
    assert_eq!(
        pending.external_interrupts().len(),
        PendingEventState::MAX_EXTERNAL_INTERRUPTS
    );
    assert_eq!(pending.dropped_external_interrupts(), 0);

    // Record the allocation size at the cap and then attempt to push far beyond it.
    let cap_at_limit = pending.external_interrupts().capacity();
    let extra = 10_000usize;
    for _ in 0..extra {
        pending.inject_external_interrupt(0x21);
    }

    assert_eq!(
        pending.external_interrupts().len(),
        PendingEventState::MAX_EXTERNAL_INTERRUPTS
    );
    assert_eq!(pending.dropped_external_interrupts(), extra as u64);
    assert_eq!(pending.external_interrupts().capacity(), cap_at_limit);
}

#[test]
fn interrupt_frame_stack_overflow_triggers_triple_fault() {
    // Configure a minimal protected-mode environment where `INT 0x80` can be
    // delivered repeatedly without stack switching.
    let mut mem = FlatTestBus::new(0x40000);

    let idt_base = 0x1000;
    write_idt_gate32(&mut mem, idt_base, 0x80, 0x08, 0x2000, 0x8E); // present, DPL0, int gate

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x30000);
    cpu.state.set_rflags(0x202);

    // Deliver up to the hard cap.
    for i in 0..PendingEventState::MAX_INTERRUPT_FRAMES {
        cpu.pending
            .raise_software_interrupt(0x80, 0x1000 + i as u64);
        cpu.deliver_pending_event(&mut mem)
            .expect("interrupt delivery should succeed below the cap");
    }

    // One more delivery would push the bookkeeping stack over the cap and should
    // fail closed with a fatal exit.
    cpu.pending.raise_software_interrupt(0x80, 0xDEAD);
    assert_eq!(
        cpu.deliver_pending_event(&mut mem),
        Err(CpuExit::TripleFault)
    );
    assert_eq!(cpu.pending.dropped_interrupt_frames(), 1);
}
