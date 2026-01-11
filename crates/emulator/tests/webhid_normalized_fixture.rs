use std::path::PathBuf;

use emulator::io::usb::hid::parse_report_descriptor;
use emulator::io::usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};

#[test]
fn webhid_normalized_fixture_deserializes_and_synthesizes_descriptor() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hid/webhid_normalized_mouse.json");
    let fixture_bytes = std::fs::read(&fixture_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", fixture_path.display()));

    let expected_json: serde_json::Value =
        serde_json::from_slice(&fixture_bytes).expect("parse fixture JSON");

    let collections: Vec<HidCollectionInfo> = serde_json::from_slice(&fixture_bytes)
        .unwrap_or_else(|err| panic!("deserialize fixture JSON: {err}"));

    // Lock down the JSON wire contract: serde -> JSON must roundtrip without
    // dropping/renaming any fields.
    let actual_json = serde_json::to_value(&collections).expect("serialize fixture JSON");
    assert_eq!(actual_json, expected_json);

    let descriptor = synthesize_report_descriptor(&collections)
        .unwrap_or_else(|err| panic!("synthesize_report_descriptor: {err}"));
    assert!(!descriptor.is_empty());

    parse_report_descriptor(&descriptor).expect("parse synthesized report descriptor");
}
