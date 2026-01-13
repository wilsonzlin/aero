#![cfg(target_arch = "wasm32")]

use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion, UsbHostCompletionIn};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};
use aero_wasm::XhciControllerBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

#[wasm_bindgen_test]
fn xhci_controller_bridge_webusb_control_in_roundtrip() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("XhciControllerBridge::new ok");
    bridge.set_connected(true);
    let mut dev = bridge.webusb_device_for_test();

    let setup = SetupPacket {
        bm_request_type: 0x80, // device-to-host | standard | device
        b_request: 0x06,       // GET_DESCRIPTOR
        w_value: 0x0100,       // DEVICE descriptor
        w_index: 0,
        w_length: 8,
    };

    let resp = dev.handle_control_request(setup, None);
    assert!(
        matches!(resp, ControlResponse::Nak),
        "expected initial control transfer attempt to return NAK while host action is in flight"
    );

    let drained = bridge.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    assert_eq!(actions.len(), 1, "expected exactly one queued host action");

    let id = match actions[0] {
        UsbHostAction::ControlIn { id, .. } => id,
        ref other => panic!("expected ControlIn action, got {other:?}"),
    };

    let completion = UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: vec![0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3],
        },
    };
    bridge
        .push_completion(serde_wasm_bindgen::to_value(&completion).unwrap())
        .expect("push_completion ok");

    let resp2 = dev.handle_control_request(setup, None);
    match resp2 {
        ControlResponse::Data(data) => assert_eq!(
            data,
            vec![0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3],
            "expected control transfer to complete once completion is pushed"
        ),
        other => panic!("expected Data after completion, got {other:?}"),
    }
}

#[wasm_bindgen_test]
fn xhci_controller_bridge_snapshot_restore_reconnects_and_resets_host_state() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);

    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 8,
    };

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("XhciControllerBridge::new ok");
    bridge.set_connected(true);
    let mut dev = bridge.webusb_device_for_test();

    assert!(matches!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak
    ));
    let drained = bridge.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    let first_id = match actions.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action, got {other:?}"),
    };

    // With no completion, repeated attempts should not re-emit host actions.
    assert!(matches!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak
    ));
    let drained_again = bridge.drain_actions().expect("drain_actions ok");
    assert!(
        drained_again.is_null(),
        "expected inflight transfer to suppress duplicate host actions"
    );

    let snapshot = bridge.save_state();

    let mut restored =
        XhciControllerBridge::new(guest_base, guest_size).expect("XhciControllerBridge::new ok");
    restored
        .load_state(&snapshot)
        .expect("load_state after snapshot ok");

    // Host queues are cleared during restore (reset_host_state_for_restore).
    let drained_after_restore = restored.drain_actions().expect("drain_actions ok");
    assert!(
        drained_after_restore.is_null(),
        "expected host-action queues to be cleared during restore"
    );

    // Retrying the transfer should now re-emit an action with a new id.
    assert!(matches!(
        restored
            .webusb_device_for_test()
            .handle_control_request(setup, None),
        ControlResponse::Nak
    ));
    let drained_retry = restored.drain_actions().expect("drain_actions ok");
    let actions_retry: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained_retry).expect("deserialize UsbHostAction[]");
    let retry_id = match actions_retry.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action after restore, got {other:?}"),
    };
    assert_ne!(
        retry_id, first_id,
        "expected re-emitted action to allocate a new id after restore"
    );
}
