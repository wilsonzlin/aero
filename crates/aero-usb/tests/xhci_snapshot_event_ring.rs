use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

fn write_erst_entry(mem: &mut dyn MemoryBus, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    mem.write_u64(erstba, seg_base);
    mem.write_u32(erstba + 8, seg_size_trbs);
    mem.write_u32(erstba + 12, 0);
}

#[test]
fn xhci_snapshot_preserves_event_ring_producer_cursor() {
    let mut mem = TestMemory::new(0x20_000);
    let mut ctrl = XhciController::new();

    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    ctrl.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut ev0 = Trb::default();
    ev0.parameter = 0xaaaa;
    ev0.set_trb_type(TrbType::PortStatusChangeEvent);
    let mut ev1 = Trb::default();
    ev1.parameter = 0xbbbb;
    ev1.set_trb_type(TrbType::PortStatusChangeEvent);

    ctrl.post_event(ev0);
    ctrl.post_event(ev1);
    ctrl.service_event_ring(&mut mem);
    assert_eq!(ctrl.pending_event_count(), 0);

    let got0 = Trb::read_from(&mut mem, ring_base);
    let got1 = Trb::read_from(&mut mem, ring_base + TRB_LEN as u64);
    assert_eq!(got0.parameter, 0xaaaa);
    assert_eq!(got1.parameter, 0xbbbb);

    let bytes = ctrl.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    let mut ev2 = Trb::default();
    ev2.parameter = 0xcccc;
    ev2.set_trb_type(TrbType::PortStatusChangeEvent);
    restored.post_event(ev2);
    restored.service_event_ring(&mut mem);
    assert_eq!(restored.pending_event_count(), 0);

    // The restored controller should continue writing at slot 2 rather than resetting to the
    // consumer pointer.
    let got2 = Trb::read_from(&mut mem, ring_base + 2 * TRB_LEN as u64);
    assert_eq!(got2.parameter, 0xcccc);
    assert!(got2.cycle(), "producer cycle bit should be preserved across snapshot");

    // Verify older entries were not overwritten.
    let got0_again = Trb::read_from(&mut mem, ring_base);
    assert_eq!(got0_again.parameter, 0xaaaa);
}

#[test]
fn xhci_snapshot_preserves_pending_events_and_drop_counter() {
    let mut ctrl = XhciController::new();

    const TOTAL_EVENTS: u64 = 1500;
    for i in 0..TOTAL_EVENTS {
        let mut evt = Trb::default();
        evt.parameter = i;
        evt.set_trb_type(TrbType::PortStatusChangeEvent);
        ctrl.post_event(evt);
    }

    let pending = ctrl.pending_event_count() as u64;
    let dropped = ctrl.dropped_event_trbs();
    assert!(dropped > 0, "expected pending event queue to be bounded");
    assert_eq!(
        pending + dropped,
        TOTAL_EVENTS,
        "pending queue + dropped counter should account for all posted events"
    );

    let bytes = ctrl.save_state();
    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(restored.pending_event_count() as u64, pending);
    assert_eq!(restored.dropped_event_trbs(), dropped);
}

