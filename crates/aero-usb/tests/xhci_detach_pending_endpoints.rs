use aero_usb::hub::UsbHubDevice;
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

mod util;
use util::{xhci_set_run, TestMemory};

#[derive(Default)]
struct DummyDevice;

impl UsbDeviceModel for DummyDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

fn make_noop_transfer(cycle: bool) -> Trb {
    let mut trb = Trb::new(0, 0, 0);
    trb.set_trb_type(TrbType::NoOp);
    trb.set_cycle(cycle);
    trb
}

#[test]
fn xhci_detach_at_path_clears_pending_endpoints() {
    let mut mem = TestMemory::new(0x8000);
    let mut ctrl = XhciController::with_port_count(1);

    ctrl.set_dcbaap(0x1000);
    xhci_set_run(&mut ctrl);
    let mut hub = UsbHubDevice::with_port_count(8);
    hub.attach(3, Box::new(DummyDevice));
    ctrl.attach_device(0, Box::new(hub));

    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    assert_ne!(slot_id, 0);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    slot_ctx
        .set_route_string_from_root_ports(&[3])
        .expect("encode route string");
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Configure a minimal transfer ring and enqueue a single No-Op transfer TRB.
    let endpoint_id = 3u8; // EP1 IN (DCI=3)
    let tr_ring = 0x2000u64;
    ctrl.set_endpoint_ring(slot_id, endpoint_id, tr_ring, true);
    make_noop_transfer(true).write_to(&mut mem, tr_ring);

    // Ring a doorbell but detach the device before any ticks run. The detach path should clear any
    // queued endpoint activations so re-attaching the device does not spuriously consume TRBs.
    ctrl.ring_doorbell(slot_id, endpoint_id);
    ctrl.detach_at_path(&[0, 3]).expect("detach_at_path");

    // Reattach a new device at the same path and rebind the existing slot.
    ctrl.attach_at_path(&[0, 3], Box::new(DummyDevice))
        .expect("attach_at_path");
    let addr2 = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr2.completion_code, CommandCompletionCode::Success);

    // Without a new doorbell, the controller must not service any endpoints.
    let work = ctrl.step_1ms(&mut mem);
    assert_eq!(work.doorbells_serviced, 0);
    assert_eq!(work.transfer_trbs_consumed, 0);

    // Once software rings a new doorbell, the endpoint should run and consume the TRB.
    ctrl.ring_doorbell(slot_id, endpoint_id);
    let work2 = ctrl.step_1ms(&mut mem);
    assert_eq!(work2.doorbells_serviced, 1);
    assert_eq!(work2.transfer_trbs_consumed, 1);

    // With no more TRBs ready, subsequent ticks should be idle again.
    let work3 = ctrl.step_1ms(&mut mem);
    assert_eq!(work3.doorbells_serviced, 0);
    assert_eq!(work3.transfer_trbs_consumed, 0);
}
