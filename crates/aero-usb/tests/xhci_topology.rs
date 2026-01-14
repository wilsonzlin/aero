use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::xhci::context::{InputContext32, InputControlContext, SlotContext};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{SetupPacket, UsbHubAttachError, UsbInResult, UsbOutResult};

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

    // Build an input context in guest memory and issue Address Device via that pointer.
    let input_ctx_base: u64 = 0x2000;
    let input_ctx = InputContext32::new(input_ctx_base);
    let mut icc = InputControlContext::default();
    // Slot context + EP0 are typically set for Address Device.
    icc.set_add_flags((1 << 0) | (1 << 1));
    input_ctx
        .write_input_control(&mut mem, &icc)
        .expect("write ICC");
    input_ctx
        .write_slot_context(&mut mem, &slot_ctx)
        .expect("write slot context");

    let completion = ctrl.address_device_input_context(&mut mem, slot_id, input_ctx_base);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    let dev = ctrl.slot_device_mut(slot_id).expect("slot must be bound");

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

#[test]
fn xhci_route_string_binds_device_behind_nested_hubs() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMemory::new(0x4000);

    // Enable Slot requires DCBAAP to be configured.
    ctrl.set_dcbaap(0x1000);

    let keyboard = UsbHidKeyboardHandle::new();

    // Build a 2-tier hub topology:
    // root port 1 -> hub1 port 5 -> hub2 port 3 -> keyboard
    let mut hub2 = UsbHubDevice::new();
    hub2.attach(3, Box::new(keyboard.clone()));

    // Default hubs only have 4 ports; allocate enough ports to use port 5.
    let mut hub1 = UsbHubDevice::with_port_count(8);
    hub1.attach(5, Box::new(hub2));
    ctrl.attach_device(0, Box::new(hub1));

    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);

    // Route String:
    // - hub1 port = 5
    // - hub2 port = 3
    //
    // Bits 3:0 are the port closest to the device, so the route encodes as hex digits "53"
    // (root→device). Reference: xHCI 1.2 §6.2.2 "Slot Context" (Route String field).
    slot_ctx.set_route_string(0x53);

    let completion = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    let dev = ctrl.slot_device_mut(slot_id).expect("slot must be bound");

    // Issue a control transfer (GET_DESCRIPTOR: Device) through EP0 and validate this is the
    // keyboard (idProduct = 0x0001), not either hub (idProduct = 0x0002).
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

#[test]
fn xhci_detach_clears_slot_device_binding() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMemory::new(0x4000);

    ctrl.set_dcbaap(0x1000);
    ctrl.attach_device(0, Box::new(UsbHidKeyboardHandle::new()));

    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let completion = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    assert!(
        ctrl.slot_device_mut(slot_id).is_some(),
        "slot must resolve to the attached device"
    );
    assert!(
        ctrl.slot_state(slot_id)
            .expect("slot state")
            .device_attached(),
        "slot state should record attached device"
    );

    ctrl.detach_device(0);

    assert!(
        ctrl.slot_device_mut(slot_id).is_none(),
        "slot must no longer resolve after port detach"
    );
    assert!(
        !ctrl
            .slot_state(slot_id)
            .expect("slot state")
            .device_attached(),
        "slot state should record detached device"
    );
}

#[test]
fn xhci_detach_at_path_clears_slot_device_binding_for_downstream_device() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMemory::new(0x4000);

    ctrl.set_dcbaap(0x1000);

    let mut hub_dev = UsbHubDevice::with_port_count(8);
    hub_dev.attach(3, Box::new(UsbHidKeyboardHandle::new()));
    ctrl.attach_device(0, Box::new(hub_dev));

    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    slot_ctx
        .set_route_string_from_root_ports(&[3])
        .expect("encode route string");
    let completion = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    assert!(ctrl.slot_device_mut(slot_id).is_some());
    assert!(
        ctrl.slot_state(slot_id)
            .expect("slot state")
            .device_attached(),
        "slot state should record attached device"
    );

    // Detach the downstream device behind the already attached hub.
    ctrl.detach_at_path(&[0, 3]).expect("detach_at_path");

    assert!(
        ctrl.slot_device_mut(slot_id).is_none(),
        "slot must no longer resolve after downstream detach"
    );
    assert!(
        !ctrl
            .slot_state(slot_id)
            .expect("slot state")
            .device_attached(),
        "slot state should record detached device"
    );

    // Re-attaching a device at the same topology path does not automatically re-bind an existing
    // slot; the guest must issue Address Device again.
    ctrl.attach_at_path(&[0, 3], Box::new(UsbHidKeyboardHandle::new()))
        .expect("attach_at_path");
    assert!(ctrl.slot_device_mut(slot_id).is_none());
}

#[test]
fn xhci_attach_at_path_rejects_downstream_port_over_15() {
    let mut ctrl = XhciController::new();
    let res = ctrl.attach_at_path(&[0, 16], Box::new(UsbHidKeyboardHandle::new()));
    assert_eq!(res, Err(UsbHubAttachError::InvalidPort));
}

#[test]
fn xhci_attach_at_path_rejects_paths_deeper_than_5_hub_tiers() {
    let mut ctrl = XhciController::new();
    // Root port + 6 downstream hubs (Route String only supports 5).
    let path = [0, 1, 1, 1, 1, 1, 1];
    let res = ctrl.attach_at_path(&path, Box::new(UsbHidKeyboardHandle::new()));
    assert_eq!(res, Err(UsbHubAttachError::InvalidPort));
}
