use aero_usb::ehci::regs as ehci_regs;
use aero_usb::ehci::EhciController;
use aero_usb::uhci::regs as uhci_regs;
use aero_usb::uhci::UhciController;
use aero_usb::xhci::regs as xhci_regs;
use aero_usb::xhci::XhciController;
use aero_usb::MemoryBus;

#[derive(Default)]
struct NoDmaCountingMem {
    reads: usize,
    writes: usize,
}

impl MemoryBus for NoDmaCountingMem {
    fn dma_enabled(&self) -> bool {
        false
    }

    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        self.reads += 1;
        buf.fill(0xFF);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        self.writes += 1;
    }
}

#[test]
fn uhci_tick_1ms_does_not_walk_schedule_without_dma() {
    let mut mem = NoDmaCountingMem::default();
    let mut uhci = UhciController::new();

    // Program a non-zero frame list base address and start the controller. Without gating this
    // would attempt to DMA-read the frame list entry each tick.
    uhci.io_write(uhci_regs::REG_FLBASEADD, 4, 0x1000);
    uhci.io_write(uhci_regs::REG_USBCMD, 2, uhci_regs::USBCMD_RS as u32);

    uhci.tick_1ms(&mut mem);

    assert_eq!(
        mem.reads, 0,
        "UHCI must not read the frame list when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "UHCI must not write TD/QH state when dma_enabled() is false"
    );

    let frnum = uhci.io_read(uhci_regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum, 1, "UHCI frame counter should advance while running");
}

#[test]
fn ehci_tick_1ms_does_not_walk_schedule_without_dma() {
    let mut mem = NoDmaCountingMem::default();
    let mut ehci = EhciController::new();

    // Program a non-zero async schedule head and enable the async schedule. Without DMA gating the
    // schedule walker would read guest memory and could set error bits based on open-bus data.
    ehci.mmio_write(ehci_regs::REG_ASYNCLISTADDR, 4, 0x1000);
    ehci.mmio_write(
        ehci_regs::REG_USBCMD,
        4,
        ehci_regs::USBCMD_RS | ehci_regs::USBCMD_ASE,
    );

    ehci.tick_1ms(&mut mem);

    assert_eq!(
        mem.reads, 0,
        "EHCI must not read schedule structures when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "EHCI must not write qTD/QH overlays when dma_enabled() is false"
    );

    // FRINDEX advances by 8 microframes per 1ms tick.
    assert_eq!(
        ehci.mmio_read(ehci_regs::REG_FRINDEX, 4),
        8,
        "EHCI microframe counter should advance while running"
    );

    // Schedule faults should not be raised while DMA is disabled.
    let usbsts = ehci.mmio_read(ehci_regs::REG_USBSTS, 4);
    assert_eq!(usbsts & ehci_regs::USBSTS_HSE, 0);
}

#[test]
fn xhci_tick_1ms_does_not_touch_guest_memory_without_dma() {
    let mut mem = NoDmaCountingMem::default();
    let mut xhci = XhciController::new();

    // Start the controller so the tick-driven CRCR probe would normally run.
    xhci.mmio_write(&mut mem, xhci_regs::REG_USBCMD, 4, xhci_regs::USBCMD_RUN);

    let mf0 = xhci.mmio_read(&mut mem, xhci_regs::REG_MFINDEX, 4) & 0x3fff;

    xhci.tick_1ms(&mut mem);

    assert_eq!(
        mem.reads, 0,
        "xHCI tick_1ms must not DMA-read guest memory when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "xHCI tick_1ms must not DMA-write guest memory when dma_enabled() is false"
    );

    let mf1 = xhci.mmio_read(&mut mem, xhci_regs::REG_MFINDEX, 4) & 0x3fff;
    assert_eq!(
        mf1,
        (mf0 + 8) & 0x3fff,
        "xHCI MFINDEX should still advance while DMA is disabled"
    );
}
