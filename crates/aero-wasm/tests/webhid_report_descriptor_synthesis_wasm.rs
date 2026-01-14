#![cfg(target_arch = "wasm32")]

use js_sys::JSON;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

// Some browser-only APIs used by `aero-wasm` are worker-only (e.g. OPFS sync access handles).
// Run wasm-bindgen tests in a worker so this integration test matches the typical web runtime.
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_worker);

#[wasm_bindgen_test]
fn synthesize_webhid_normalized_mouse_descriptor_matches_expected_bytes() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    ));
    let collections = JSON::parse(fixture).expect("parse webhid_normalized_mouse.json fixture");

    let descriptor = aero_wasm::synthesize_webhid_report_descriptor(collections)
        .expect("synthesize descriptor from JS value");

    let mut bytes = vec![0u8; descriptor.length() as usize];
    descriptor.copy_to(&mut bytes);

    // Expected descriptor bytes for the normalized mouse fixture.
    //
    // Note: WebHID collection metadata does not always include the extra nested
    // Pointer/Physical collection or a wheel axis. This fixture is intentionally
    // minimal: 3 buttons + 2x 8-bit relative axes (X/Y).
    let expected: [u8; 55] = [
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xA1, 0x01, // Collection (Application)
        0x05, 0x09, // Usage Page (Buttons)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x35, 0x00, // Physical Minimum (0)
        0x45, 0x01, // Physical Maximum (1)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x01, // Report Size (1)
        0x95, 0x03, // Report Count (3)
        0x19, 0x01, // Usage Minimum (Button 1)
        0x29, 0x03, // Usage Maximum (Button 3)
        0x81, 0x02, // Input (Data,Var,Abs) Button bits
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x15, 0x81, // Logical Minimum (-127)
        0x25, 0x7F, // Logical Maximum (127)
        0x35, 0x81, // Physical Minimum (-127)
        0x45, 0x7F, // Physical Maximum (127)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x09, 0x30, // Usage (X)
        0x09, 0x31, // Usage (Y)
        0x81, 0x06, // Input (Data,Var,Rel) X,Y
        0xC0, // End Collection
    ];

    assert_eq!(bytes, expected.to_vec());
}

#[wasm_bindgen_test]
fn synthesize_webhid_normalized_keyboard_descriptor_matches_expected_bytes() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_keyboard.json"
    ));
    let collections = JSON::parse(fixture).expect("parse webhid_normalized_keyboard.json fixture");

    let descriptor = aero_wasm::synthesize_webhid_report_descriptor(collections)
        .expect("synthesize descriptor from JS value");

    let mut bytes = vec![0u8; descriptor.length() as usize];
    descriptor.copy_to(&mut bytes);

    // Expected descriptor bytes for the normalized keyboard fixture.
    //
    // Layout mirrors a minimal USB HID boot keyboard:
    // - 8 one-bit modifier keys (E0..E7)
    // - 1 reserved byte (constant)
    // - 6 keycode bytes (array, 0..101)
    // - 5 LED output bits + 3 bits padding
    let expected: [u8; 119] = [
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x06, // Usage (Keyboard)
        0xA1, 0x01, // Collection (Application)
        0x05, 0x07, // Usage Page (Keyboard/Keypad)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x35, 0x00, // Physical Minimum (0)
        0x45, 0x01, // Physical Maximum (1)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x01, // Report Size (1)
        0x95, 0x08, // Report Count (8)
        0x19, 0xE0, // Usage Minimum (Keyboard LeftControl)
        0x29, 0xE7, // Usage Maximum (Keyboard Right GUI)
        0x81, 0x02, // Input (Data,Var,Abs) Modifiers
        0x05, 0x07, // Usage Page (Keyboard/Keypad)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x35, 0x00, // Physical Minimum (0)
        0x45, 0x01, // Physical Maximum (1)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x08, // Report Size (8)
        0x95, 0x01, // Report Count (1)
        0x81, 0x03, // Input (Const,Var,Abs) Reserved
        0x05, 0x07, // Usage Page (Keyboard/Keypad)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x65, // Logical Maximum (101)
        0x35, 0x00, // Physical Minimum (0)
        0x45, 0x65, // Physical Maximum (101)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x08, // Report Size (8)
        0x95, 0x06, // Report Count (6)
        0x19, 0x00, // Usage Minimum (0)
        0x29, 0x65, // Usage Maximum (101)
        0x81, 0x00, // Input (Data,Array,Abs) Keys
        0x05, 0x08, // Usage Page (LEDs)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x35, 0x00, // Physical Minimum (0)
        0x45, 0x01, // Physical Maximum (1)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x01, // Report Size (1)
        0x95, 0x05, // Report Count (5)
        0x19, 0x01, // Usage Minimum (Num Lock)
        0x29, 0x05, // Usage Maximum (Kana)
        0x91, 0x02, // Output (Data,Var,Abs) LEDs
        0x05, 0x08, // Usage Page (LEDs)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x35, 0x00, // Physical Minimum (0)
        0x45, 0x01, // Physical Maximum (1)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x03, // Report Size (3)
        0x95, 0x01, // Report Count (1)
        0x91, 0x03, // Output (Const,Var,Abs) LED padding
        0xC0, // End Collection
    ];

    assert_eq!(bytes, expected.to_vec());
}

#[wasm_bindgen_test]
fn synthesize_webhid_normalized_gamepad_descriptor_matches_expected_bytes() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_gamepad.json"
    ));
    let collections = JSON::parse(fixture).expect("parse webhid_normalized_gamepad.json fixture");

    let descriptor = aero_wasm::synthesize_webhid_report_descriptor(collections)
        .expect("synthesize descriptor from JS value");

    let mut bytes = vec![0u8; descriptor.length() as usize];
    descriptor.copy_to(&mut bytes);

    // Expected descriptor bytes for the normalized gamepad fixture.
    //
    // Minimal gamepad: 8 buttons + 2x 8-bit absolute axes (X/Y).
    let expected: [u8; 55] = [
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x05, // Usage (Game Pad)
        0xA1, 0x01, // Collection (Application)
        0x05, 0x09, // Usage Page (Buttons)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x35, 0x00, // Physical Minimum (0)
        0x45, 0x01, // Physical Maximum (1)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x01, // Report Size (1)
        0x95, 0x08, // Report Count (8)
        0x19, 0x01, // Usage Minimum (Button 1)
        0x29, 0x08, // Usage Maximum (Button 8)
        0x81, 0x02, // Input (Data,Var,Abs) Buttons
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x15, 0x81, // Logical Minimum (-127)
        0x25, 0x7F, // Logical Maximum (127)
        0x35, 0x81, // Physical Minimum (-127)
        0x45, 0x7F, // Physical Maximum (127)
        0x55, 0x00, // Unit Exponent (0)
        0x65, 0x00, // Unit (None)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x09, 0x30, // Usage (X)
        0x09, 0x31, // Usage (Y)
        0x81, 0x02, // Input (Data,Var,Abs) X,Y
        0xC0, // End Collection
    ];

    assert_eq!(bytes, expected.to_vec());
}

#[wasm_bindgen_test]
fn synthesize_webhid_report_descriptor_error_includes_field_path() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    ));
    let collections = JSON::parse(fixture).expect("parse webhid_normalized_mouse.json fixture");

    // Introduce a schema error: `usagePage` must be a number, but we set it to a string.
    let arr = js_sys::Array::from(&collections);
    let first = arr.get(0);
    js_sys::Reflect::set(
        &first,
        &wasm_bindgen::JsValue::from_str("usagePage"),
        &wasm_bindgen::JsValue::from_str("not-a-number"),
    )
    .expect("Reflect::set should succeed");

    let err = aero_wasm::synthesize_webhid_report_descriptor(collections)
        .expect_err("expected schema error");
    let msg = err
        .as_string()
        .or_else(|| {
            err.dyn_ref::<js_sys::Error>()
                .and_then(|e| e.message().as_string())
        })
        .unwrap_or_else(|| format!("{err:?}"));

    assert!(
        msg.contains("at [0].usagePage"),
        "expected error message to include field path; got: {msg}"
    );
}
