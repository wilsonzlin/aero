use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::interrupter::IMAN_IE;
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
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (ERST_BASE >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, EVENT_RING_BASE as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (EVENT_RING_BASE >> 32) as u32);
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
