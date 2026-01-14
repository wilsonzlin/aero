use aero_usb::hid::webhid::{
    HidCollectionInfo, HidCollectionType, HidReportInfo, HidReportItem, infer_boot_interface,
    max_output_report_bytes,
};

fn parse_fixture_collections(json: &str) -> Vec<HidCollectionInfo> {
    serde_json::from_str(json).expect("fixture JSON should deserialize")
}

fn make_item(report_size: u32, report_count: u32) -> HidReportItem {
    HidReportItem {
        usage_page: 0,
        usages: vec![],
        usage_minimum: 0,
        usage_maximum: 0,
        report_size,
        report_count,
        unit_exponent: 0,
        unit: 0,
        logical_minimum: 0,
        logical_maximum: 0,
        physical_minimum: 0,
        physical_maximum: 0,
        strings: vec![],
        string_minimum: 0,
        string_maximum: 0,
        designators: vec![],
        designator_minimum: 0,
        designator_maximum: 0,
        is_absolute: true,
        is_array: false,
        is_buffered_bytes: false,
        is_constant: false,
        is_linear: true,
        is_range: false,
        is_relative: false,
        is_volatile: false,
        has_null: false,
        has_preferred_state: true,
        is_wrapped: false,
    }
}

fn make_output_collection(report_id: u32, payload_bytes: u32) -> HidCollectionInfo {
    HidCollectionInfo {
        usage_page: 0x01,
        usage: 0,
        collection_type: HidCollectionType::Application,
        children: vec![],
        input_reports: vec![],
        output_reports: vec![HidReportInfo {
            report_id,
            items: vec![make_item(8, payload_bytes)],
        }],
        feature_reports: vec![],
    }
}

#[test]
fn infer_boot_interface_from_fixtures() {
    let mouse = parse_fixture_collections(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    )));
    assert_eq!(infer_boot_interface(&mouse), Some((1, 2)));

    let keyboard = parse_fixture_collections(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_keyboard.json"
    )));
    assert_eq!(infer_boot_interface(&keyboard), Some((1, 1)));

    let gamepad = parse_fixture_collections(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_gamepad.json"
    )));
    assert_eq!(infer_boot_interface(&gamepad), None);
}

#[test]
fn infer_boot_interface_returns_none_for_keyboard_mouse_combo() {
    let keyboard = parse_fixture_collections(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_keyboard.json"
    )));
    let mouse = parse_fixture_collections(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    )));

    let mut combined = Vec::new();
    combined.extend(keyboard);
    combined.extend(mouse);

    assert_eq!(infer_boot_interface(&combined), None);
}

#[test]
fn max_output_report_bytes_computes_on_wire_length() {
    // report_id==0 -> no report-id prefix.
    let small = vec![make_output_collection(0, 2)];
    assert_eq!(max_output_report_bytes(&small), 2);

    // report_id!=0 adds a 1-byte report-id prefix.
    let large_with_id = vec![make_output_collection(1, 64)];
    assert_eq!(max_output_report_bytes(&large_with_id), 65);
}

#[test]
fn max_output_report_bytes_aggregates_across_collections_per_report_id() {
    let a = make_output_collection(0, 40);
    let b = make_output_collection(0, 30);

    assert_eq!(max_output_report_bytes(&[a, b]), 70);
}

