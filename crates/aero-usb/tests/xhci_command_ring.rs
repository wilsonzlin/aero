use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

mod util;

use util::TestMemory;

#[test]
fn command_ring_noop_then_enable_slot_emits_completion_events() {
    const CMD_RING_BASE: u64 = 0x1000;
    const ERST_BASE: u64 = 0x2000;
    const EVENT_RING_BASE: u64 = 0x3000;
    const DCBAAP_BASE: u64 = 0x4000;

    let mut mem = TestMemory::new(0x10_000);

    // Command ring: [NoOpCmd] [EnableSlotCmd] [Link -> base, TC=1]
    let mut noop = Trb::default();
    noop.set_cycle(true);
    noop.set_trb_type(TrbType::NoOpCommand);
    noop.write_to(&mut mem, CMD_RING_BASE);

    let mut enable = Trb::default();
    enable.set_cycle(true);
    enable.set_trb_type(TrbType::EnableSlotCommand);
    enable.write_to(&mut mem, CMD_RING_BASE + TRB_LEN as u64);

    let mut link = Trb::default();
    link.parameter = CMD_RING_BASE;
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(true);
    link.write_to(&mut mem, CMD_RING_BASE + 2 * TRB_LEN as u64);

    // Event Ring Segment Table (single entry).
    mem.write_physical(ERST_BASE, &(EVENT_RING_BASE as u64).to_le_bytes());
    mem.write_physical(ERST_BASE + 8, &(8u32).to_le_bytes()); // segment size in TRBs
    mem.write_physical(ERST_BASE + 12, &0u32.to_le_bytes());

    let mut xhci = XhciController::new();

    // Configure DCBAAP so Enable Slot can succeed.
    xhci.mmio_write(&mut mem, regs::REG_DCBAAP_LO, 4, DCBAAP_BASE as u32);
    xhci.mmio_write(&mut mem, regs::REG_DCBAAP_HI, 4, (DCBAAP_BASE >> 32) as u32);

    // Configure interrupter 0 event ring.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, ERST_BASE as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERSTBA_HI,
        4,
        (ERST_BASE >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, EVENT_RING_BASE as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (EVENT_RING_BASE >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Program the command ring dequeue pointer + cycle state.
    xhci.mmio_write(
        &mut mem,
        regs::REG_CRCR_LO,
        4,
        (CMD_RING_BASE as u32) | 1, // RCS=1
    );
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (CMD_RING_BASE >> 32) as u32);

    // Start the controller and ring doorbell 0 to process commands.
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);
    // Ensure completion events are flushed to the guest event ring.
    xhci.service_event_ring(&mut mem);

    let ev0 = Trb::read_from(&mut mem, EVENT_RING_BASE);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev0.parameter, CMD_RING_BASE);
    assert_eq!(ev0.completion_code_raw(), 1); // Success
    assert_eq!(ev0.slot_id(), 0);

    let ev1 = Trb::read_from(&mut mem, EVENT_RING_BASE + TRB_LEN as u64);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.parameter, CMD_RING_BASE + TRB_LEN as u64);
    assert_eq!(ev1.completion_code_raw(), 1); // Success
    assert_eq!(ev1.slot_id(), 1);
}

#[test]
fn command_ring_kick_persists_until_ring_empty_and_requires_doorbell0() {
    const CMD_RING_BASE: u64 = 0x1000;
    const ERST_BASE: u64 = 0x2000;
    const EVENT_RING_BASE: u64 = 0x3000;
    const CMD_COUNT: usize = 32;

    // Provide enough room for the command ring + event ring.
    let mut mem = TestMemory::new(0x40_000);

    // Command ring: N x [NoOpCmd] then a stop marker with cycle mismatch.
    for i in 0..CMD_COUNT {
        let mut noop = Trb::default();
        noop.set_cycle(true);
        noop.set_trb_type(TrbType::NoOpCommand);
        noop.write_to(&mut mem, CMD_RING_BASE + (i as u64) * (TRB_LEN as u64));
    }
    // Stop marker: cycle mismatch => ring appears empty after consuming N TRBs.
    {
        let mut stop = Trb::default();
        stop.set_cycle(false);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.write_to(
            &mut mem,
            CMD_RING_BASE + (CMD_COUNT as u64) * (TRB_LEN as u64),
        );
    }

    // Event Ring Segment Table (single entry).
    mem.write_physical(ERST_BASE, &(EVENT_RING_BASE as u64).to_le_bytes());
    mem.write_physical(ERST_BASE + 8, &(64u32).to_le_bytes()); // segment size in TRBs
    mem.write_physical(ERST_BASE + 12, &0u32.to_le_bytes());

    let mut xhci = XhciController::new();

    // Configure interrupter 0 event ring.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, ERST_BASE as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERSTBA_HI,
        4,
        (ERST_BASE >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, EVENT_RING_BASE as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (EVENT_RING_BASE >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Program the command ring dequeue pointer + cycle state.
    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, (CMD_RING_BASE as u32) | 1);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (CMD_RING_BASE >> 32) as u32);

    // Start controller + ring doorbell0 once.
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);

    // Command processing is bounded per MMIO access; ensure the controller keeps making progress
    // across subsequent MMIO reads without requiring another doorbell ring.
    let mut remaining = CMD_COUNT;
    for _ in 0..128 {
        // Any MMIO read should advance the command ring if cmd_kick is still set.
        let _ = xhci.mmio_read(&mut mem, regs::REG_USBCMD, 4);
        xhci.service_event_ring(&mut mem);

        remaining = (0..CMD_COUNT)
            .filter(|&i| {
                Trb::read_from(&mut mem, EVENT_RING_BASE + (i as u64) * (TRB_LEN as u64)).trb_type()
                    != TrbType::CommandCompletionEvent
            })
            .count();
        if remaining == 0 {
            break;
        }
    }
    assert_eq!(
        remaining, 0,
        "expected all command completions to be delivered"
    );

    for i in 0..CMD_COUNT {
        let ev = Trb::read_from(&mut mem, EVENT_RING_BASE + (i as u64) * (TRB_LEN as u64));
        assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
        assert_eq!(ev.parameter, CMD_RING_BASE + (i as u64) * (TRB_LEN as u64));
        assert_eq!(ev.completion_code_raw(), 1); // Success
    }

    // Allow the controller to observe the cycle mismatch and transition to the idle state.
    for _ in 0..4 {
        let _ = xhci.mmio_read(&mut mem, regs::REG_USBCMD, 4);
        xhci.service_event_ring(&mut mem);
    }
    let ev_before_new_command = Trb::read_from(
        &mut mem,
        EVENT_RING_BASE + (CMD_COUNT as u64) * (TRB_LEN as u64),
    );
    assert_eq!(ev_before_new_command.trb_type(), TrbType::Unknown(0));

    // Once the ring appears empty, the controller should stop polling it until doorbell0 is rung
    // again. Verify that by adding another command without ringing doorbell0.
    let next_cmd_addr = CMD_RING_BASE + (CMD_COUNT as u64) * (TRB_LEN as u64);
    {
        let mut noop = Trb::default();
        noop.set_cycle(true);
        noop.set_trb_type(TrbType::NoOpCommand);
        noop.write_to(&mut mem, next_cmd_addr);
    }

    // Without ringing doorbell0, no additional completion event should be produced.
    for _ in 0..16 {
        let _ = xhci.mmio_read(&mut mem, regs::REG_USBCMD, 4);
        xhci.service_event_ring(&mut mem);
    }
    let ev_without_doorbell = Trb::read_from(
        &mut mem,
        EVENT_RING_BASE + (CMD_COUNT as u64) * (TRB_LEN as u64),
    );
    assert_eq!(ev_without_doorbell.trb_type(), TrbType::Unknown(0));

    // Ring doorbell0 again to kick command processing.
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);
    for _ in 0..16 {
        let _ = xhci.mmio_read(&mut mem, regs::REG_USBCMD, 4);
        xhci.service_event_ring(&mut mem);
    }

    let ev_after_doorbell = Trb::read_from(
        &mut mem,
        EVENT_RING_BASE + (CMD_COUNT as u64) * (TRB_LEN as u64),
    );
    assert_eq!(
        ev_after_doorbell.trb_type(),
        TrbType::CommandCompletionEvent
    );
    assert_eq!(ev_after_doorbell.parameter, next_cmd_addr);
    assert_eq!(ev_after_doorbell.completion_code_raw(), 1);
}
