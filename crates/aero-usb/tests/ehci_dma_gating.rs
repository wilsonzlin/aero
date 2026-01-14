use aero_usb::ehci::{regs, EhciController};
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
fn ehci_tick_does_not_process_schedules_without_dma() {
    let mut ctrl = EhciController::new();

    // Configure an asynchronous schedule and start the controller. If the schedule engine runs it
    // would attempt to fetch QH/qTD state from guest memory.
    ctrl.mmio_write(regs::REG_ASYNCLISTADDR, 4, 0x1000);
    ctrl.mmio_write(
        regs::REG_USBINTR,
        4,
        regs::USBINTR_USBINT | regs::USBINTR_USBERRINT,
    );
    ctrl.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    let fr0 = ctrl.mmio_read(regs::REG_FRINDEX, 4) & regs::FRINDEX_MASK;

    let mut mem = NoDmaPanicMem::default();
    ctrl.tick_1ms(&mut mem);

    // FRINDEX should still advance even though schedule processing is skipped.
    let fr1 = ctrl.mmio_read(regs::REG_FRINDEX, 4) & regs::FRINDEX_MASK;
    assert_eq!(fr1, (fr0 + 8) & regs::FRINDEX_MASK);

    let usbsts = ctrl.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(
        usbsts & (regs::USBSTS_USBINT | regs::USBSTS_USBERRINT),
        0,
        "schedule processing must not set interrupt status while DMA is disabled"
    );
    assert!(!ctrl.irq_level());
}

