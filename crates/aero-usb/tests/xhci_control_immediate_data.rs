use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::xhci::transfer::Ep0TransferEngine;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

mod util;

use util::{Alloc, TestMemory};

const TRB_CYCLE: u32 = 1 << 0;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_TRB_TYPE_SHIFT: u32 = Trb::CONTROL_TRB_TYPE_SHIFT;
const TRB_DIR_IN: u32 = 1 << 16;

// Setup Stage TRB TRT field (bits 16..=17).
const SETUP_TRT_OUT: u32 = 2 << 16;
const SETUP_TRT_IN: u32 = 3 << 16;

#[derive(Clone)]
struct CaptureControlOutDevice {
    captured: Rc<RefCell<Vec<u8>>>,
}

impl UsbDeviceModel for CaptureControlOutDevice {
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        assert_eq!(setup.bm_request_type, 0x40);
        assert_eq!(setup.b_request, 0x55);
        assert_eq!(setup.w_value, 0);
        assert_eq!(setup.w_index, 0);
        assert_eq!(setup.w_length, 4);

        let data = data_stage.expect("expected OUT data stage");
        self.captured.borrow_mut().clear();
        self.captured.borrow_mut().extend_from_slice(data);
        ControlResponse::Ack
    }
}

struct ImmediateControlInDevice;

impl UsbDeviceModel for ImmediateControlInDevice {
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        assert_eq!(setup.bm_request_type, 0xC0);
        assert_eq!(setup.b_request, 0x66);
        assert_eq!(setup.w_value, 0);
        assert_eq!(setup.w_index, 0);
        assert_eq!(setup.w_length, 4);
        ControlResponse::Data(vec![0x11, 0x22, 0x33, 0x44])
    }
}

#[test]
fn xhci_control_out_immediate_data_stage_delivers_payload() {
    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate and configure an event ring with space for 16 event TRBs.
    let event_ring_base = alloc.alloc(16 * 16, 16);

    // Allocate a transfer ring with 4 TRBs: Setup/Data/Status/Link.
    let tr_ring_base = alloc.alloc(4 * 16, 16);
    let setup_trb_addr = tr_ring_base;
    let data_trb_addr = tr_ring_base + 16;
    let status_trb_addr = tr_ring_base + 32;
    let link_trb_addr = tr_ring_base + 48;

    // Control OUT request with a 4-byte DATA stage.
    let setup_bytes = [0x40u8, 0x55, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00];
    let setup_control = TRB_CYCLE
        | TRB_IOC
        | ((u32::from(TrbType::SetupStage.raw())) << TRB_TRB_TYPE_SHIFT)
        | SETUP_TRT_OUT;
    Trb::new(u64::from_le_bytes(setup_bytes), 8, setup_control)
        .write_to(&mut mem, setup_trb_addr as u64);

    // Data Stage: OUT immediate data (IDT=1).
    let payload = [0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0];
    let data_control = TRB_CYCLE
        | TRB_IOC
        | TRB_IDT
        | ((u32::from(TrbType::DataStage.raw())) << TRB_TRB_TYPE_SHIFT);
    Trb::new(u64::from_le_bytes(payload), 4, data_control).write_to(&mut mem, data_trb_addr as u64);

    // Status stage: IN ZLP with IOC.
    let status_control = TRB_CYCLE
        | TRB_IOC
        | TRB_DIR_IN
        | ((u32::from(TrbType::StatusStage.raw())) << TRB_TRB_TYPE_SHIFT);
    Trb::new(0, 0, status_control).write_to(&mut mem, status_trb_addr as u64);

    // Link TRB back to start with toggle-cycle.
    let mut link = Trb::new(tr_ring_base as u64, 0, 0);
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(true);
    link.write_to(&mut mem, link_trb_addr as u64);

    let captured = Rc::new(RefCell::new(Vec::new()));
    let dev = CaptureControlOutDevice {
        captured: captured.clone(),
    };

    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.set_event_ring(event_ring_base as u64, 16);
    xhci.hub_mut().attach(0, Box::new(dev));

    let slot_id = xhci.enable_slot(0).expect("slot must be allocated");
    assert!(xhci.configure_ep0(slot_id, tr_ring_base as u64, true, 64));

    xhci.ring_doorbell(&mut mem, slot_id, 1);

    assert_eq!(&*captured.borrow(), &[0xde, 0xad, 0xbe, 0xef]);

    // IOC on each stage should produce three transfer events (Setup/Data/Status).
    let evt0 = Trb::read_from(&mut mem, event_ring_base as u64);
    let evt1 = Trb::read_from(&mut mem, event_ring_base as u64 + 16);
    let evt2 = Trb::read_from(&mut mem, event_ring_base as u64 + 32);

    for (evt, ptr) in [
        (evt0, setup_trb_addr),
        (evt1, data_trb_addr),
        (evt2, status_trb_addr),
    ] {
        assert_eq!(evt.trb_type(), TrbType::TransferEvent);
        assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
        assert_eq!(evt.status & 0x00ff_ffff, 0);
        assert_eq!(evt.parameter, ptr as u64);
    }
}

#[test]
fn xhci_control_in_immediate_data_stage_writes_trb_parameter() {
    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate and configure an event ring with space for 16 event TRBs.
    let event_ring_base = alloc.alloc(16 * 16, 16);

    // Allocate a transfer ring with 4 TRBs: Setup/Data/Status/Link.
    let tr_ring_base = alloc.alloc(4 * 16, 16);
    let setup_trb_addr = tr_ring_base;
    let data_trb_addr = tr_ring_base + 16;
    let status_trb_addr = tr_ring_base + 32;
    let link_trb_addr = tr_ring_base + 48;

    // Control IN request with a 4-byte DATA stage.
    let setup_bytes = [0xC0u8, 0x66, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00];
    let setup_control = TRB_CYCLE
        | TRB_IOC
        | ((u32::from(TrbType::SetupStage.raw())) << TRB_TRB_TYPE_SHIFT)
        | SETUP_TRT_IN;
    Trb::new(u64::from_le_bytes(setup_bytes), 8, setup_control)
        .write_to(&mut mem, setup_trb_addr as u64);

    // Data Stage: IN immediate data (IDT=1).
    let data_control = TRB_CYCLE
        | TRB_IOC
        | TRB_IDT
        | TRB_DIR_IN
        | ((u32::from(TrbType::DataStage.raw())) << TRB_TRB_TYPE_SHIFT);
    Trb::new(0, 4, data_control).write_to(&mut mem, data_trb_addr as u64);

    // Status stage: OUT ZLP with IOC.
    let status_control =
        TRB_CYCLE | TRB_IOC | ((u32::from(TrbType::StatusStage.raw())) << TRB_TRB_TYPE_SHIFT);
    Trb::new(0, 0, status_control).write_to(&mut mem, status_trb_addr as u64);

    // Link TRB back to start with toggle-cycle.
    let mut link = Trb::new(tr_ring_base as u64, 0, 0);
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(true);
    link.write_to(&mut mem, link_trb_addr as u64);

    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.set_event_ring(event_ring_base as u64, 16);
    xhci.hub_mut().attach(0, Box::new(ImmediateControlInDevice));

    let slot_id = xhci.enable_slot(0).expect("slot must be allocated");
    assert!(xhci.configure_ep0(slot_id, tr_ring_base as u64, true, 64));

    xhci.ring_doorbell(&mut mem, slot_id, 1);

    // The controller should write the response bytes into the DataStage TRB parameter field.
    let data_trb = Trb::read_from(&mut mem, data_trb_addr as u64);
    assert_eq!(
        data_trb.parameter.to_le_bytes(),
        [0x11, 0x22, 0x33, 0x44, 0, 0, 0, 0]
    );

    // IOC on each stage should produce three transfer events (Setup/Data/Status).
    let evt0 = Trb::read_from(&mut mem, event_ring_base as u64);
    let evt1 = Trb::read_from(&mut mem, event_ring_base as u64 + 16);
    let evt2 = Trb::read_from(&mut mem, event_ring_base as u64 + 32);

    for (evt, ptr) in [
        (evt0, setup_trb_addr),
        (evt1, data_trb_addr),
        (evt2, status_trb_addr),
    ] {
        assert_eq!(evt.trb_type(), TrbType::TransferEvent);
        assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
        assert_eq!(evt.status & 0x00ff_ffff, 0);
        assert_eq!(evt.parameter, ptr as u64);
    }
}
