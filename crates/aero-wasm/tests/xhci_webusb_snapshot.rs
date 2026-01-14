#![cfg(target_arch = "wasm32")]

use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion, UsbHostCompletionIn};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};
use aero_wasm::XhciControllerBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

#[wasm_bindgen_test]
fn xhci_controller_bridge_snapshot_is_deterministic_and_roundtrips_with_webusb() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");
    bridge.set_connected(true);

    let snap1 = bridge.snapshot_state().to_vec();
    assert!(
        snap1.len() > 16,
        "expected snapshot to contain at least the header + state fields"
    );

    let snap2 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap2, "snapshot bytes should be deterministic");

    bridge.restore_state(&snap1).expect("restore_state ok");

    let snap3 = bridge.snapshot_state().to_vec();
    assert_eq!(snap1, snap3, "snapshot should roundtrip");
}

#[wasm_bindgen_test]
fn xhci_webusb_passthrough_drain_actions_and_push_completion_roundtrip() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");
    bridge.set_connected(true);

    let mut dev = bridge.webusb_device_for_test();

    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 1,
    };

    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak,
        "first attempt should queue a host action"
    );

    let drained = bridge.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    let (id, host_setup) = match actions.as_slice() {
        [UsbHostAction::ControlIn { id, setup }] => (*id, *setup),
        other => panic!("expected exactly one ControlIn action, got {other:?}"),
    };
    assert_eq!(host_setup.bm_request_type, setup.bm_request_type);
    assert_eq!(host_setup.b_request, setup.b_request);
    assert_eq!(host_setup.w_value, setup.w_value);
    assert_eq!(host_setup.w_index, setup.w_index);
    assert_eq!(host_setup.w_length, setup.w_length);

    let completion = UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: vec![0x12, 0x34],
        },
    };
    bridge
        .push_completion(serde_wasm_bindgen::to_value(&completion).unwrap())
        .unwrap();

    // Retrying the same control request should now surface the completion, with payload truncated
    // to `wLength`.
    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Data(vec![0x12]),
    );

    let drained_again = bridge.drain_actions().expect("drain_actions ok");
    assert!(
        drained_again.is_null(),
        "expected no queued actions after completion consumed"
    );
}

#[wasm_bindgen_test]
fn xhci_webusb_restore_clears_host_state_and_allows_retry() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");
    bridge.set_connected(true);

    let mut dev = bridge.webusb_device_for_test();
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 1,
    };

    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    let drained = bridge.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    let first_id = match actions.as_slice() {
        [UsbHostAction::ControlIn { id, .. }] => *id,
        other => panic!("expected exactly one ControlIn action, got {other:?}"),
    };

    // Drain the action without pushing a completion; the request remains inflight.
    let snapshot = bridge.snapshot_state().to_vec();

    let mut bridge2 =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");
    bridge2.restore_state(&snapshot).expect("restore_state ok");

    // Snapshot restore must clear host-side passthrough state so in-flight promises don't wedge.
    let drained_after_restore = bridge2.drain_actions().expect("drain_actions ok");
    assert!(
        drained_after_restore.is_null(),
        "expected WebUSB host queues to be cleared on restore"
    );

    // With host state cleared but `next_id` preserved, retrying should allocate a new action id.
    let mut dev2 = bridge2.webusb_device_for_test();
    assert_eq!(
        dev2.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    let drained_retry = bridge2.drain_actions().expect("drain_actions ok");
    let actions_retry: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained_retry).expect("deserialize UsbHostAction[]");
    let retry_id = match actions_retry.as_slice() {
        [UsbHostAction::ControlIn { id, .. }] => *id,
        other => panic!("expected exactly one ControlIn action after restore, got {other:?}"),
    };

    assert_ne!(
        retry_id, first_id,
        "expected re-emitted host action to allocate a new id after restore"
    );
}

