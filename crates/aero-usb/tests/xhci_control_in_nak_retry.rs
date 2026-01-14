use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::xhci::transfer::Ep0TransferEngine;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

mod util;

use util::{Alloc, TestMemory};

const TRB_CYCLE: u32 = 1 << 0;
const TRB_IOC: u32 = 1 << 5;
const TRB_TRB_TYPE_SHIFT: u32 = Trb::CONTROL_TRB_TYPE_SHIFT;
const TRB_DIR_IN: u32 = 1 << 16;

#[derive(Default)]
struct ControlInState {
    now_ms: u64,
}

#[derive(Clone)]
struct DelayedControlInDevice {
    state: Rc<RefCell<ControlInState>>,
}

impl DelayedControlInDevice {
    fn new(state: Rc<RefCell<ControlInState>>) -> Self {
        Self { state }
    }
}

impl UsbDeviceModel for DelayedControlInDevice {
    fn tick_1ms(&mut self) {
        self.state.borrow_mut().now_ms += 1;
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        // Only implement the single request used by the test.
        assert_eq!(setup.bm_request_type, 0xC0);
        assert_eq!(setup.b_request, 0x01);
        assert_eq!(setup.w_length, 4);

        let now = self.state.borrow().now_ms;
        if now == 0 {
            ControlResponse::Nak
        } else {
            ControlResponse::Data(vec![0x11, 0x22, 0x33, 0x44])
        }
    }
}

#[test]
fn xhci_control_in_nak_retries_until_ready() {
    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate and configure an event ring with space for 8 event TRBs.
    let event_ring_base = alloc.alloc(8 * 16, 16);

    // Allocate a transfer ring with 4 TRBs: Setup/Data/Status/Link.
    let tr_ring_base = alloc.alloc(4 * 16, 16);
    let setup_trb_addr = tr_ring_base;
    let data_trb_addr = tr_ring_base + 16;
    let status_trb_addr = tr_ring_base + 32;
    let link_trb_addr = tr_ring_base + 48;

    // Data buffer for response (4 bytes).
    let data_buf = alloc.alloc(4, 16);

    // Vendor control-IN request (bmRequestType=0xC0, bRequest=0x01, wLength=4).
    let setup_bytes = [0xC0u8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00];
    let setup_control = TRB_CYCLE
        | ((u32::from(TrbType::SetupStage.raw())) << TRB_TRB_TYPE_SHIFT)
        // TRT=IN (bits 16..=17).
        | (3 << 16);
    let setup_trb = Trb::new(u64::from_le_bytes(setup_bytes), 8, setup_control);
    setup_trb.write_to(&mut mem, setup_trb_addr as u64);

    let data_control =
        TRB_CYCLE | ((u32::from(TrbType::DataStage.raw())) << TRB_TRB_TYPE_SHIFT) | TRB_DIR_IN;
    let data_trb = Trb::new(data_buf as u64, 4, data_control);
    data_trb.write_to(&mut mem, data_trb_addr as u64);

    let status_control =
        TRB_CYCLE | ((u32::from(TrbType::StatusStage.raw())) << TRB_TRB_TYPE_SHIFT) | TRB_IOC;
    let status_trb = Trb::new(0, 0, status_control);
    status_trb.write_to(&mut mem, status_trb_addr as u64);

    let mut link = Trb::new(tr_ring_base as u64, 0, 0);
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(true);
    link.write_to(&mut mem, link_trb_addr as u64);

    let state = Rc::new(RefCell::new(ControlInState::default()));
    let dev = DelayedControlInDevice::new(state.clone());

    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.set_event_ring(event_ring_base as u64, 8);
    xhci.hub_mut().attach(0, Box::new(dev));

    let slot_id = xhci.enable_slot(0).expect("slot must be allocated");
    assert!(xhci.configure_ep0(slot_id, tr_ring_base as u64, true, 64));

    // First doorbell should observe NAK and leave the DATA/STATUS TRBs pending.
    xhci.ring_doorbell(&mut mem, slot_id, 1);

    let mut buf = [0u8; 4];
    mem.read_physical(data_buf as u64, &mut buf);
    assert_eq!(buf, [0; 4], "guest buffer must not be written until ready");

    let evt = Trb::read_from(&mut mem, event_ring_base as u64);
    assert_ne!(
        evt.trb_type(),
        TrbType::TransferEvent,
        "transfer event must not be produced while NAKed"
    );

    // Advance time by 1ms; device will become ready and the transfer should complete.
    xhci.tick_1ms(&mut mem);

    mem.read_physical(data_buf as u64, &mut buf);
    assert_eq!(buf, [0x11, 0x22, 0x33, 0x44]);

    let evt = Trb::read_from(&mut mem, event_ring_base as u64);
    assert_eq!(evt.trb_type(), TrbType::TransferEvent);
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt.status & 0x00ff_ffff, 0);
    assert_eq!(evt.parameter, status_trb_addr as u64);

    // Sanity: ensure our fake device observed the tick.
    assert_eq!(state.borrow().now_ms, 1);
}
