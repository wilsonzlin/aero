use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn xhci_snapshot_roundtrip_preserves_pending_events() {
    let mut xhci = XhciController::new();

    let mut evt = Trb::default();
    evt.parameter = 0xdead_beef;
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    assert_eq!(xhci.pending_event_count(), 1);

    let bytes = xhci.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    assert_eq!(restored.pending_event_count(), 1);

    // Program an event ring and verify the restored pending event is delivered.
    let mut mem = TestMemory::new(0x20_000);
    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    restored.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    restored.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    restored.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERSTBA_HI,
        4,
        (erstba >> 32) as u32,
    );
    restored.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    restored.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (ring_base >> 32) as u32,
    );
    restored.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    restored.service_event_ring(&mut mem);

    let got = Trb::read_from(&mut mem, ring_base);
    assert_eq!(got.trb_type(), TrbType::PortStatusChangeEvent);
    assert_eq!(got.parameter, 0xdead_beef);
    assert!(restored.interrupter0().interrupt_pending());
}

#[test]
fn xhci_snapshot_roundtrip_preserves_dropped_event_counter() {
    let mut xhci = XhciController::new();

    for i in 0..5000u64 {
        let mut evt = Trb::default();
        evt.parameter = i;
        evt.set_trb_type(TrbType::PortStatusChangeEvent);
        xhci.post_event(evt);
        if xhci.dropped_event_trbs() != 0 {
            break;
        }
    }

    assert_ne!(
        xhci.dropped_event_trbs(),
        0,
        "expected to drop at least one event TRB"
    );
    let dropped = xhci.dropped_event_trbs();
    let pending = xhci.pending_event_count();

    let bytes = xhci.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(restored.dropped_event_trbs(), dropped);
    assert_eq!(restored.pending_event_count(), pending);
}
