use std::path::PathBuf;

use emulator::io::usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};

#[test]
fn webhid_normalized_fixture_deserializes_and_synthesizes_descriptor() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hid/webhid_normalized_mouse.json");
    let fixture_bytes = std::fs::read(&fixture_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", fixture_path.display()));

    let collections: Vec<HidCollectionInfo> = serde_json::from_slice(&fixture_bytes)
        .unwrap_or_else(|err| panic!("deserialize fixture JSON: {err}"));

    let descriptor = synthesize_report_descriptor(&collections)
        .unwrap_or_else(|err| panic!("synthesize_report_descriptor: {err}"));
    assert!(!descriptor.is_empty());
}

