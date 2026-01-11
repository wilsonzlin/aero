use std::path::PathBuf;

use aero_usb::hid::webhid::HidCollectionInfo;

#[test]
fn synthesize_webhid_normalized_mouse_descriptor_matches_expected_bytes() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hid/webhid_normalized_mouse.json");
    let fixture_bytes =
        std::fs::read(&fixture_path).expect("read webhid_normalized_mouse.json fixture");

    let collections: Vec<HidCollectionInfo> =
        serde_json::from_slice(&fixture_bytes).expect("deserialize normalized collections");

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
