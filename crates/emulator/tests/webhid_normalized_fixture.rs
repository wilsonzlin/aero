use std::path::PathBuf;

use emulator::io::usb::hid::parse_report_descriptor;
use emulator::io::usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};

#[test]
fn webhid_normalized_fixtures_roundtrip_and_synthesize_descriptor_via_emulator_reexports() {
    // NOTE: The canonical WebHID schema + synthesis tests live in `crates/aero-usb`
    // (see ADR 0015). This test exists to ensure `crates/emulator` continues to
    // provide a stable `emulator::io::usb::hid` re-export path for consumers.
    for fixture_name in [
        "webhid_normalized_mouse.json",
        "webhid_normalized_keyboard.json",
        "webhid_normalized_gamepad.json",
    ] {
        let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../tests/fixtures/hid/{fixture_name}"));
        let fixture_bytes = std::fs::read(&fixture_path)
            .unwrap_or_else(|err| panic!("read {}: {err}", fixture_path.display()));

        let expected_json: serde_json::Value =
            serde_json::from_slice(&fixture_bytes).expect("parse fixture JSON");

        let collections: Vec<HidCollectionInfo> = serde_json::from_slice(&fixture_bytes)
            .unwrap_or_else(|err| panic!("deserialize fixture JSON: {err}"));

        // Lock down the JSON wire contract: serde -> JSON must roundtrip without
        // dropping/renaming any fields.
        let actual_json = serde_json::to_value(&collections).expect("serialize fixture JSON");
        assert_eq!(
            actual_json, expected_json,
            "fixture roundtrip mismatch: {fixture_name}"
        );

        let descriptor = synthesize_report_descriptor(&collections)
            .unwrap_or_else(|err| panic!("synthesize_report_descriptor ({fixture_name}): {err}"));
        assert!(
            !descriptor.is_empty(),
            "empty report descriptor: {fixture_name}"
        );

        parse_report_descriptor(&descriptor)
            .unwrap_or_else(|err| panic!("parse synthesized descriptor ({fixture_name}): {err}"));
    }
}
