use std::path::PathBuf;

use aero_usb::hid::webhid::HidCollectionInfo;

fn load_fixture(name: &str) -> Vec<HidCollectionInfo> {
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../tests/fixtures/hid/{name}"));
    let fixture_bytes =
        std::fs::read(&fixture_path).unwrap_or_else(|err| panic!("read {name}: {err}"));

    serde_json::from_slice(&fixture_bytes).expect("deserialize normalized collections")
}

#[test]
fn synthesize_webhid_normalized_mouse_descriptor_matches_expected_bytes() {
    let collections = load_fixture("webhid_normalized_mouse.json");

    let descriptor = aero_wasm::synthesize_webhid_report_descriptor_bytes(&collections)
        .expect("synthesize descriptor");

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

    assert_eq!(descriptor, expected.to_vec());
}

#[test]
fn synthesize_webhid_normalized_keyboard_descriptor_matches_expected_bytes() {
    let collections = load_fixture("webhid_normalized_keyboard.json");

    let descriptor = aero_wasm::synthesize_webhid_report_descriptor_bytes(&collections)
        .expect("synthesize descriptor");

    // Expected descriptor bytes for the normalized keyboard fixture.
    //
    // Note: WebHID collection metadata is per-report-item; our synthesizer emits a deterministic
    // descriptor by re-stating the full set of HID globals (usage page, logical/physical ranges,
    // report size/count, ...) for each main item.
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

    assert_eq!(descriptor, expected.to_vec());
}

#[test]
fn synthesize_webhid_normalized_gamepad_descriptor_matches_expected_bytes() {
    let collections = load_fixture("webhid_normalized_gamepad.json");

    let descriptor = aero_wasm::synthesize_webhid_report_descriptor_bytes(&collections)
        .expect("synthesize descriptor");

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

    assert_eq!(descriptor, expected.to_vec());
}
