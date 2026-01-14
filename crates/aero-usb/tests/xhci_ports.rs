use std::cell::Cell;
use std::rc::Rc;

use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{
    regs, XhciController, PORTSC_CCS, PORTSC_CSC, PORTSC_PEC, PORTSC_PED, PORTSC_PR, PORTSC_PRC,
};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

#[derive(Default)]
struct PanicMem;

impl MemoryBus for PanicMem {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write");
    }
}

#[derive(Clone)]
struct DummyDevice {
    reset_count: Rc<Cell<u32>>,
}

impl UsbDeviceModel for DummyDevice {
    fn reset(&mut self) {
        self.reset_count.set(self.reset_count.get() + 1);
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

fn assert_portsc_has(portsc: u32, bit: u32) {
    assert!(
        portsc & bit != 0,
        "expected PORTSC to have bit {bit:#x}, got {portsc:#x}"
    );
}

fn assert_portsc_not(portsc: u32, bit: u32) {
    assert!(
        portsc & bit == 0,
        "expected PORTSC to not have bit {bit:#x}, got {portsc:#x}"
    );
}

#[test]
fn xhci_ports_attach_reset_and_events() {
    let mut xhci = XhciController::new();
    let mut mem = PanicMem;
    let portsc_off = regs::port::portsc_offset(0);

    let reset_count = Rc::new(Cell::new(0));
    xhci.attach_device(
        0,
        Box::new(DummyDevice {
            reset_count: reset_count.clone(),
        }),
    );

    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_has(portsc, PORTSC_CCS);
    assert_portsc_has(portsc, PORTSC_CSC);

    // Attach should generate a Port Status Change Event TRB.
    let ev = xhci.pop_pending_event().expect("expected event after attach");
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = ((ev.dword0() >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Clear CSC via write-1-to-clear.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_CSC);
    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_not(portsc, PORTSC_CSC);

    // Trigger port reset.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_PR);
    assert_eq!(reset_count.get(), 1, "device reset should be called once");

    // Wait ~50ms for reset to complete.
    for _ in 0..50 {
        xhci.tick_1ms();
    }

    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_not(portsc, PORTSC_PR);
    assert_portsc_has(portsc, PORTSC_PED);
    assert_portsc_has(portsc, PORTSC_PEC);
    assert_portsc_has(portsc, PORTSC_PRC);

    // Reset completion should also generate a port status change event.
    let ev = xhci
        .pop_pending_event()
        .expect("expected event after reset completion");
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = ((ev.dword0() >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Clear PEC/PRC so we can observe a second reset completion while the port is already enabled.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_PEC | PORTSC_PRC);

    // Trigger a second reset while the port is enabled. This sets PEC and queues a PSC event.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_PR);
    assert_eq!(reset_count.get(), 2, "device reset should be called twice");

    let ev = xhci
        .pop_pending_event()
        .expect("expected event after reset begin (enabled port)");
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = ((ev.dword0() >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Wait for reset completion. PEC is already set, so PRC must drive the PSC event.
    for _ in 0..50 {
        xhci.tick_1ms();
    }

    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_not(portsc, PORTSC_PR);
    assert_portsc_has(portsc, PORTSC_PED);
    assert_portsc_has(portsc, PORTSC_PRC);

    let ev = xhci
        .pop_pending_event()
        .expect("expected event after reset completion (enabled port)");
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = ((ev.dword0() >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // No extra events.
    assert_eq!(xhci.pop_pending_event(), None);

    // Sanity: TRB is stable POD.
    let _trb_layout: Trb = ev;
}
