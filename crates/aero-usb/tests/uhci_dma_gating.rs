use aero_usb::uhci::{regs, UhciController};
use aero_usb::MemoryBus;

#[derive(Default)]
struct NoDmaPanicMem;

impl MemoryBus for NoDmaPanicMem {
    fn dma_enabled(&self) -> bool {
        false
    }

    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read while dma_enabled=false");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write while dma_enabled=false");
    }
}

#[test]
fn uhci_tick_does_not_walk_schedule_without_dma() {
    let mut ctrl = UhciController::new();

    // Point FLBASEADD at a non-zero frame list base so schedule processing would attempt to DMA.
    ctrl.io_write(regs::REG_FLBASEADD, 4, 0x1000);
    ctrl.io_write(regs::REG_FRNUM, 2, 0);

    // Start the controller.
    ctrl.io_write(regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

    let fr0 = ctrl.io_read(regs::REG_FRNUM, 2) as u16;

    let mut mem = NoDmaPanicMem::default();
    ctrl.tick_1ms(&mut mem);

    // Frame number should still advance even though schedule processing is skipped.
    let fr1 = ctrl.io_read(regs::REG_FRNUM, 2) as u16;
    assert_eq!(fr1, fr0.wrapping_add(1) & 0x07ff);

    let usbsts = ctrl.io_read(regs::REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts & (regs::USBSTS_USBINT | regs::USBSTS_USBERRINT),
        0,
        "schedule processing must not set interrupt status while DMA is disabled"
    );
    assert!(!ctrl.irq_level());
}
