use aero_usb::xhci::interrupter::{IMAN_IE, IMAN_IP};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController, EVENT_ENQUEUE_BUDGET_PER_TICK};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn event_ring_enqueue_writes_trb_and_sets_interrupt_pending() {
    let mut mem = TestMemory::new(0x20_000);

    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut evt = Trb::default();
    evt.parameter = 0x1234_5678;
    evt.set_trb_type(TrbType::PortStatusChangeEvent);

    xhci.post_event(evt);
    xhci.tick_1ms_and_service_event_ring(&mut mem);

    let got = Trb::read_from(&mut mem, ring_base);
    assert!(got.cycle(), "controller should set the producer cycle bit");
    assert_eq!(got.trb_type(), TrbType::PortStatusChangeEvent);
    assert_eq!(got.parameter, 0x1234_5678);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    // Verify IMAN.IP is W1C.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IP | IMAN_IE);
    assert!(!xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());
}

#[test]
fn event_ring_wrap_and_budget_are_bounded() {
    let mut mem = TestMemory::new(0x40_000);

    let erstba = 0x1000;
    let ring_base = 0x8000;
    // One segment with enough space for `EVENT_ENQUEUE_BUDGET_PER_TICK + 1` TRBs.
    write_erst_entry(
        &mut mem,
        erstba,
        ring_base,
        (EVENT_ENQUEUE_BUDGET_PER_TICK as u32) + 1,
    );

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    for i in 0..(EVENT_ENQUEUE_BUDGET_PER_TICK + 1) {
        let mut evt = Trb::default();
        evt.parameter = i as u64;
        evt.set_trb_type(TrbType::PortStatusChangeEvent);
        xhci.post_event(evt);
    }

    xhci.service_event_ring(&mut mem);

    // Only the budgeted number of events should have been written.
    assert_eq!(xhci.pending_event_count(), 1);

    let last_written_addr = ring_base + (EVENT_ENQUEUE_BUDGET_PER_TICK as u64) * (TRB_LEN as u64);
    let last = Trb::read_from(&mut mem, last_written_addr);
    assert!(
        !last.cycle(),
        "TRB just beyond the enqueue budget should still be empty/zeroed"
    );
}
