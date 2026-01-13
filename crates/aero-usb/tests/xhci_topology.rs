use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

mod util;

use util::TestMemory;

#[test]
fn xhci_route_string_binds_device_behind_external_hub() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMemory::new(0x4000);

    // Enable Slot requires DCBAAP to be configured.
    ctrl.set_dcbaap(0x1000);

    let mut hub_dev = UsbHubDevice::new();
    hub_dev.attach(3, Box::new(UsbHidKeyboardHandle::new()));
    ctrl.attach_device(0, Box::new(hub_dev));

    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    // RouteString tier 1 = 3, RootHubPortNumber = 1.
    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    slot_ctx
        .set_route_string_from_root_ports(&[3])
        .expect("encode route string");

    let completion = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    let dev = ctrl
        .slot_device_mut(slot_id)
        .expect("slot must be bound");

    // Issue a control transfer (GET_DESCRIPTOR: Device) through EP0 and validate this is the
    // keyboard (idProduct = 0x0001), not the hub (idProduct = 0x0002).
    let setup = SetupPacket {
        bm_request_type: 0x80, // DeviceToHost | Standard | Device
        b_request: 0x06,       // GET_DESCRIPTOR
        w_value: 0x0100,       // Device descriptor
        w_index: 0,
        w_length: 18,
    };

    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    let desc = match dev.handle_in(0, 64) {
        UsbInResult::Data(data) => data,
        other => panic!("expected device descriptor bytes, got {other:?}"),
    };
    assert_eq!(desc.len(), 18);
    let id_product = u16::from_le_bytes([desc[10], desc[11]]);
    assert_eq!(id_product, 0x0001);
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
}
