use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

mod util;

use util::{Alloc, TestMemory};

#[test]
fn xhci_event_ring_misaligned_erst_entry_base_sets_hce() {
    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let erstba = alloc.alloc(16, 0x10) as u64;
    let event_ring_base = alloc.alloc((TRB_LEN as u32) * 8, 0x10) as u64;

    // ERST entry with a misaligned base address (reserved low bits set).
    MemoryBus::write_u64(&mut mem, erstba, event_ring_base | 0x0f);
    MemoryBus::write_u32(&mut mem, erstba + 8, 8);
    MemoryBus::write_u32(&mut mem, erstba + 12, 0);

    let mut xhci = XhciController::new();
    // Configure interrupter 0 event ring.
    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erstba >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, event_ring_base);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, event_ring_base >> 32);

    // Queue an event and attempt delivery.
    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    xhci.service_event_ring(&mut mem);

    let sts = xhci.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_ne!(sts & regs::USBSTS_HCE, 0, "controller should latch HCE");

    // The controller must not mask the ERST entry base address down and write events into guest
    // memory anyway.
    let mut bytes = [0u8; TRB_LEN];
    mem.read_physical(event_ring_base, &mut bytes);
    assert_eq!(
        bytes,
        [0u8; TRB_LEN],
        "event ring should remain untouched when ERST entry base is misaligned"
    );
}

