use std::cell::Cell;
use std::rc::Rc;

use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{XhciController, PORTSC_CCS, PORTSC_CSC, PORTSC_PED, PORTSC_PR};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

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

    let reset_count = Rc::new(Cell::new(0));
    xhci.attach_device(
        0,
        Box::new(DummyDevice {
            reset_count: reset_count.clone(),
        }),
    );

    let portsc = xhci.read_portsc(0);
    assert_portsc_has(portsc, PORTSC_CCS);
    assert_portsc_has(portsc, PORTSC_CSC);

    // Attach should generate a Port Status Change Event TRB.
    let ev = xhci.pop_pending_event().expect("expected event after attach");
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = ((ev.dword0() >> 24) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Clear CSC via write-1-to-clear.
    xhci.write_portsc(0, PORTSC_CSC);
    let portsc = xhci.read_portsc(0);
    assert_portsc_not(portsc, PORTSC_CSC);

    // Trigger port reset.
    xhci.write_portsc(0, PORTSC_PR);
    assert_eq!(reset_count.get(), 1, "device reset should be called once");

    // Wait ~50ms for reset to complete.
    for _ in 0..50 {
        xhci.tick_1ms();
    }

    let portsc = xhci.read_portsc(0);
    assert_portsc_not(portsc, PORTSC_PR);
    assert_portsc_has(portsc, PORTSC_PED);

    // Reset completion should also generate a port status change event.
    let ev = xhci
        .pop_pending_event()
        .expect("expected event after reset completion");
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = ((ev.dword0() >> 24) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // No extra events.
    assert_eq!(xhci.pop_pending_event(), None);

    // Sanity: TRB is stable POD.
    let _trb_layout: Trb = ev;
}
