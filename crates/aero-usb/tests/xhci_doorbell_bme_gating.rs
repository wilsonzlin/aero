use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

#[derive(Default)]
struct OpenBusNoDma;

impl MemoryBus for OpenBusNoDma {
    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        buf.fill(0xff);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}

    fn dma_enabled(&self) -> bool {
        false
    }
}

#[test]
fn xhci_doorbell_does_not_service_event_ring_when_dma_disabled() {
    let mut mem = OpenBusNoDma;
    let mut xhci = XhciController::with_port_count(1);

    // Configure interrupter 0 so the controller believes an event ring is programmed, but perform
    // all accesses through an open-bus/no-DMA memory bus.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, 0x1000);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, 0x2000);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, 0);

    // Queue an event that would require interpreting the ERST/ERDP state to deliver.
    let mut trb = Trb::default();
    trb.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(trb);
    assert_eq!(xhci.pending_event_count(), 1);

    // Ring a non-zero doorbell. Previously, this attempted to drain the pending event into the
    // guest event ring even when DMA was disabled, which could spuriously set USBSTS.HCE due to
    // open-bus reads of the ERST.
    let doorbell1 = u64::from(regs::DBOFF_VALUE) + u64::from(regs::doorbell::DOORBELL_STRIDE);
    xhci.mmio_write(&mut mem, doorbell1, 4, 3);

    let usbsts = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_eq!(
        usbsts & regs::USBSTS_HCE,
        0,
        "doorbell write must not interpret ERST state while DMA is disabled"
    );
    assert_eq!(
        xhci.pending_event_count(),
        1,
        "pending events should remain queued while DMA is disabled"
    );
}

