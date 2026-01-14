//! Budgeting tests for the xHCI controller `step_1ms` implementation.
//!
//! These tests ensure a malicious guest cannot force unbounded ring walking or command processing in
//! a single 1ms frame: the controller must do at most the configured amount of work per tick while
//! still making forward progress across ticks.

use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{budget, regs, XhciController};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

fn write_trb(mem: &mut impl MemoryBus, addr: u64, trb: Trb) {
    trb.write_to(mem, addr);
}

fn make_noop_command(cycle: bool) -> Trb {
    let mut trb = Trb::new(0, 0, 0);
    trb.set_trb_type(TrbType::NoOpCommand);
    trb.set_slot_id(0);
    trb.set_cycle(cycle);
    trb
}

fn count_event_trbs(mem: &mut impl MemoryBus, base: u64, max: usize) -> usize {
    let mut count = 0usize;
    for i in 0..max {
        let addr = base + (i as u64) * (TRB_LEN as u64);
        let trb = Trb::read_from(mem, addr);
        if !trb.cycle() {
            break;
        }
        count += 1;
    }
    count
}

#[test]
fn xhci_step_1ms_command_ring_is_bounded_and_makes_progress() {
    // Large enough to exceed any per-tick budget by orders of magnitude.
    const COMMAND_TRBS: usize = 10_000;

    // Guest structures.
    let cmd_ring_base: u64 = 0x10_000;
    let erstba: u64 = 0x08_000;
    let event_ring_base: u64 = 0x40_000;
    let event_ring_trbs: u16 =
        u16::try_from(COMMAND_TRBS + 1).expect("event ring size fits in u16");

    let mem_size = (event_ring_base + (event_ring_trbs as u64) * (TRB_LEN as u64) + 0x1000) as usize;
    let mut mem = TestMemory::new(mem_size);

    write_erst_entry(&mut mem, erstba, event_ring_base, event_ring_trbs as u32);

    // Command ring: many No-Op commands with cycle=1 followed by a cycle-mismatch sentinel TRB.
    for i in 0..COMMAND_TRBS {
        write_trb(
            &mut mem,
            cmd_ring_base + (i as u64) * (TRB_LEN as u64),
            make_noop_command(true),
        );
    }
    // Sentinel TRB: cycle=0 (ring empty).
    write_trb(
        &mut mem,
        cmd_ring_base + (COMMAND_TRBS as u64) * (TRB_LEN as u64),
        make_noop_command(false),
    );

    let mut ctrl = XhciController::new();
    ctrl.set_command_ring(cmd_ring_base, true);

    // Configure interrupter 0 to deliver events into our guest event ring.
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, event_ring_base as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (event_ring_base >> 32) as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Start the controller so command ring processing is enabled.
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // Ring the command doorbell (doorbell 0) without triggering the MMIO-side command processing
    // fast path; this lets the test assert that `step_1ms` itself is properly budgeted.
    ctrl.write_doorbell(0, 0);

    // First tick: must not process more than the configured per-frame budget.
    let work = ctrl.step_1ms(&mut mem);
    assert!(
        work.command_trbs_processed <= budget::MAX_COMMAND_TRBS_PER_FRAME,
        "command TRB budget exceeded: {} > {}",
        work.command_trbs_processed,
        budget::MAX_COMMAND_TRBS_PER_FRAME
    );
    assert!(
        work.event_trbs_written <= budget::MAX_EVENT_TRBS_PER_FRAME,
        "event budget exceeded: {} > {}",
        work.event_trbs_written,
        budget::MAX_EVENT_TRBS_PER_FRAME
    );

    let events_after_one = count_event_trbs(&mut mem, event_ring_base, event_ring_trbs as usize);
    assert_eq!(
        events_after_one,
        budget::MAX_COMMAND_TRBS_PER_FRAME,
        "expected exactly one tick worth of events"
    );

    // Run enough ticks to drain the work.
    let ticks_needed = (COMMAND_TRBS + budget::MAX_COMMAND_TRBS_PER_FRAME - 1)
        / budget::MAX_COMMAND_TRBS_PER_FRAME;
    for _ in 1..ticks_needed {
        ctrl.step_1ms(&mut mem);
    }

    let events_final = count_event_trbs(&mut mem, event_ring_base, event_ring_trbs as usize);
    assert_eq!(events_final, COMMAND_TRBS, "expected all commands to complete");
}
