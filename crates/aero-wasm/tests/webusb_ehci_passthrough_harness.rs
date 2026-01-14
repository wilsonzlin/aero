#![cfg(target_arch = "wasm32")]

use aero_usb::passthrough::UsbHostAction;
use aero_wasm::WebUsbEhciPassthroughHarness;
use wasm_bindgen_test::wasm_bindgen_test;

fn drain_single_control_in_id(harness: &mut WebUsbEhciPassthroughHarness) -> u32 {
    for _ in 0..32 {
        harness.tick();
        let drained = harness.drain_actions().expect("drain_actions ok");
        if drained.is_null() {
            continue;
        }
        let actions: Vec<UsbHostAction> =
            serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
        let first = actions.first().expect("expected at least one action");
        return match first {
            UsbHostAction::ControlIn { id, .. } => *id,
            other => panic!("expected ControlIn action, got {other:?}"),
        };
    }
    panic!("expected harness to queue a host action");
}

#[wasm_bindgen_test]
fn webusb_ehci_harness_action_ids_monotonic_across_detach_attach() {
    let mut harness = WebUsbEhciPassthroughHarness::new();
    harness.attach_controller();
    harness.attach_device().expect("attach_device ok");

    harness
        .cmd_get_device_descriptor()
        .expect("cmd_get_device_descriptor ok");
    let first_id = drain_single_control_in_id(&mut harness);

    harness.detach_device();
    let drained = harness.drain_actions().expect("drain_actions ok");
    assert!(
        drained.is_null(),
        "expected drain_actions to return null when the device is detached"
    );

    harness.attach_device().expect("attach_device ok (reattach)");
    harness
        .cmd_get_device_descriptor()
        .expect("cmd_get_device_descriptor ok (reattach)");
    let second_id = drain_single_control_in_id(&mut harness);

    assert!(
        second_id > first_id,
        "expected action ids to be monotonic across detach/attach (first={first_id}, second={second_id})"
    );
}

#[wasm_bindgen_test]
fn webusb_ehci_harness_action_ids_monotonic_across_controller_detach_attach() {
    let mut harness = WebUsbEhciPassthroughHarness::new();
    harness.attach_controller();
    harness.attach_device().expect("attach_device ok");

    harness
        .cmd_get_device_descriptor()
        .expect("cmd_get_device_descriptor ok");
    let first_id = drain_single_control_in_id(&mut harness);

    harness.detach_controller();
    harness.attach_controller();
    harness.attach_device().expect("attach_device ok (after reattach)");

    harness
        .cmd_get_device_descriptor()
        .expect("cmd_get_device_descriptor ok (after reattach)");
    let second_id = drain_single_control_in_id(&mut harness);

    assert!(
        second_id > first_id,
        "expected action ids to be monotonic across controller detach/attach (first={first_id}, second={second_id})"
    );
}

