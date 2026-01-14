use aero_usb::ehci::regs as ehci_regs;
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::uhci::regs as uhci_regs;
use aero_usb::uhci::UhciController;
use aero_usb::xhci::regs as xhci_regs;
use aero_usb::xhci::transfer::{Ep0TransferEngine, XhciTransferExecutor};
use aero_usb::xhci::trb::{Trb, TrbType};
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
    xhci.mmio_write(xhci_regs::REG_USBCMD, 4, u64::from(xhci_regs::USBCMD_RUN));

    let mf0 = xhci.mmio_read(xhci_regs::REG_MFINDEX, 4) & 0x3fff;

    xhci.tick_1ms(&mut mem);

    assert_eq!(
        mem.reads, 0,
        "xHCI tick_1ms must not DMA-read guest memory when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "xHCI tick_1ms must not DMA-write guest memory when dma_enabled() is false"
    );

    let mf1 = xhci.mmio_read(xhci_regs::REG_MFINDEX, 4) & 0x3fff;
    assert_eq!(
        mf1,
        (mf0 + 8) & 0x3fff,
        "xHCI MFINDEX should still advance while DMA is disabled"
    );
}

#[test]
fn xhci_process_command_ring_does_not_touch_guest_memory_without_dma() {
    let mut mem = NoDmaCountingMem::default();
    let mut xhci = XhciController::new();

    // Program a non-zero command ring pointer so the helper would normally DMA-read TRBs.
    xhci.set_command_ring(0x1000, true);

    // `process_command_ring` should bail out before touching guest memory when DMA is disabled.
    let ring_empty = xhci.process_command_ring(&mut mem, 8);
    assert!(
        ring_empty,
        "expected process_command_ring to report no progress without DMA"
    );
    assert_eq!(
        mem.reads, 0,
        "xHCI process_command_ring must not DMA-read guest memory when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "xHCI process_command_ring must not DMA-write guest memory when dma_enabled() is false"
    );
    assert_eq!(
        xhci.pending_event_count(),
        0,
        "xHCI process_command_ring must not synthesize command completion events without DMA"
    );
}

#[test]
fn xhci_service_event_ring_does_not_touch_guest_memory_without_dma() {
    let mut mem = NoDmaCountingMem::default();
    let mut xhci = XhciController::new();

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);

    xhci.service_event_ring(&mut mem);

    assert_eq!(
        mem.reads, 0,
        "xHCI service_event_ring must not DMA-read guest memory when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "xHCI service_event_ring must not DMA-write guest memory when dma_enabled() is false"
    );
    assert_eq!(
        xhci.pending_event_count(),
        1,
        "xHCI service_event_ring should not drain host-queued events while DMA is disabled"
    );
    assert!(
        !xhci.interrupter0().interrupt_pending(),
        "xHCI should not assert interrupter pending while DMA is disabled"
    );
}

#[test]
fn xhci_transfer_executor_does_not_touch_guest_memory_without_dma() {
    let mut mem = NoDmaCountingMem::default();
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard));

    // Configure a dummy endpoint pointing at an arbitrary transfer ring pointer. Without DMA gating
    // the executor would attempt to read a TRB (open-bus 0xFF) and halt the endpoint with TRB Error.
    exec.add_endpoint(0x81, 0x1000);

    exec.tick_1ms(&mut mem);
    exec.poll_endpoint(&mut mem, 0x81);

    assert_eq!(
        mem.reads, 0,
        "xHCI transfer executor must not DMA-read guest memory when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "xHCI transfer executor must not DMA-write guest memory when dma_enabled() is false"
    );
    assert!(exec.take_events().is_empty());
    let st = exec.endpoint_state(0x81).expect("endpoint exists");
    assert!(!st.halted);
    assert_eq!(st.ring.dequeue_ptr, 0x1000);
}

#[test]
fn xhci_ep0_transfer_engine_does_not_touch_guest_memory_without_dma() {
    let mut mem = NoDmaCountingMem::default();
    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.set_event_ring(0x2000, 8);
    xhci.hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));

    let slot_id = xhci.enable_slot(0).expect("slot allocation");
    assert!(xhci.configure_ep0(slot_id, 0x1000, true, 64));

    xhci.ring_doorbell(&mut mem, slot_id, 1);
    xhci.tick_1ms(&mut mem);

    assert_eq!(
        mem.reads, 0,
        "EP0 transfer engine must not DMA-read guest memory when dma_enabled() is false"
    );
    assert_eq!(
        mem.writes, 0,
        "EP0 transfer engine must not DMA-write guest memory when dma_enabled() is false"
    );
}
