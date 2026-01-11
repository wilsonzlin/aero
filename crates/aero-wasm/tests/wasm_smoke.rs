#![cfg(target_arch = "wasm32")]

use aero_wasm::{add, demo_render_rgba8888};
use aero_wasm::WebUsbUhciPassthroughHarness;
use aero_usb::passthrough::UsbHostAction;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn module_loads_and_exports_work() {
    assert_eq!(add(40, 2), 42);

    let mut buf = vec![0u8; 8 * 8 * 4];
    let offset = buf.as_mut_ptr() as u32;
    let written = demo_render_rgba8888(offset, 8, 8, 8 * 4, 1000.0);
    assert_eq!(written, 64);
    assert_eq!(&buf[0..4], &[60, 35, 20, 255]);
}

#[wasm_bindgen_test]
fn webusb_uhci_harness_queues_actions_without_host() {
    let mut harness = WebUsbUhciPassthroughHarness::new();

    // Tick long enough to finish the UHCI port reset window and reach the first
    // control transfer (GET_DESCRIPTOR device 8 bytes).
    for _ in 0..60 {
        harness.tick();
    }

    let drained = harness.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");

    assert!(!actions.is_empty(), "expected at least one queued UsbHostAction");
    // We should start enumeration with a control IN descriptor request.
    assert!(matches!(actions[0], UsbHostAction::ControlIn { .. }));
}
