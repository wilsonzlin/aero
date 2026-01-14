use aero_usb::device::{AttachedUsbDevice, UsbOutResult};
use aero_usb::hid::passthrough::{UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};
use aero_usb::hid::report_descriptor::parse_report_descriptor;
use aero_usb::hid::webhid::{
    infer_boot_interface_subclass_protocol, synthesize_report_descriptor, HidCollectionInfo,
    HidCollectionType, HidReportInfo, HidReportItem,
};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

fn parse_fixture_collections(json: &str) -> Vec<HidCollectionInfo> {
    serde_json::from_str(json).expect("fixture JSON should deserialize")
}

fn fixture_mouse_collections() -> Vec<HidCollectionInfo> {
    parse_fixture_collections(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    )))
}

fn make_minimal_item(usage_page: u32, usage: u32) -> HidReportItem {
    HidReportItem {
        usage_page,
        usages: vec![usage],
        usage_minimum: 0,
        usage_maximum: 0,
        report_size: 8,
        report_count: 1,
        unit_exponent: 0,
        unit: 0,
        logical_minimum: 0,
        logical_maximum: 255,
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

fn keyboard_collections() -> Vec<HidCollectionInfo> {
    vec![HidCollectionInfo {
        usage_page: 0x01, // Generic Desktop
        usage: 0x06,      // Keyboard
        collection_type: HidCollectionType::Application,
        children: vec![],
        input_reports: vec![HidReportInfo {
            report_id: 0,
            items: vec![make_minimal_item(0x07, 0x04)], // Keyboard/Keypad: 'a'
        }],
        output_reports: vec![],
        feature_reports: vec![],
    }]
}

#[test]
fn webhid_fixture_json_roundtrips_and_synthesizes_descriptor() {
    for (fixture_name, fixture_json) in [
        (
            "webhid_normalized_mouse.json",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
            )),
        ),
        (
            "webhid_normalized_keyboard.json",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/hid/webhid_normalized_keyboard.json"
            )),
        ),
        (
            "webhid_normalized_gamepad.json",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/hid/webhid_normalized_gamepad.json"
            )),
        ),
    ] {
        let collections = parse_fixture_collections(fixture_json);

        // Lock down the JSON wire contract: serde -> JSON must roundtrip without dropping/renaming any
        // fields. This should match the output of `web/src/hid/webhid_normalize.ts`.
        let expected_json: serde_json::Value =
            serde_json::from_str(fixture_json).expect("fixture JSON should parse");
        let actual_json = serde_json::to_value(&collections).expect("fixture should serialize");
        assert_eq!(
            actual_json, expected_json,
            "fixture roundtrip mismatch: {fixture_name}"
        );

        let desc = synthesize_report_descriptor(&collections).unwrap_or_else(|err| {
            panic!("report descriptor synthesis should succeed ({fixture_name}): {err}")
        });
        assert!(
            !desc.is_empty(),
            "expected a non-empty report descriptor: {fixture_name}"
        );

        // The synthesized bytes must also be parseable by our HID descriptor parser.
        parse_report_descriptor(&desc).unwrap_or_else(|err| {
            panic!("synthesized report descriptor should parse ({fixture_name}): {err}")
        });
    }
}

#[test]
fn webhid_infers_boot_mouse_from_fixture() {
    let collections = fixture_mouse_collections();
    assert_eq!(
        infer_boot_interface_subclass_protocol(&collections),
        Some((0x01, 0x02))
    );
}

#[test]
fn webhid_infers_boot_keyboard_from_simple_collections() {
    let collections = keyboard_collections();
    assert_eq!(
        infer_boot_interface_subclass_protocol(&collections),
        Some((0x01, 0x01))
    );
}

#[test]
fn webhid_does_not_infer_boot_protocol_when_both_keyboard_and_mouse_present() {
    let mut collections = keyboard_collections();
    collections.extend(fixture_mouse_collections());
    assert_eq!(infer_boot_interface_subclass_protocol(&collections), None);
}

fn parse_interface_descriptor_fields(bytes: &[u8]) -> Option<(u8, u8)> {
    const INTERFACE_DESC_OFFSET: usize = 9;
    if bytes.len() < INTERFACE_DESC_OFFSET + 9 {
        return None;
    }
    // Config descriptor is always followed immediately by a single interface descriptor.
    if bytes[INTERFACE_DESC_OFFSET] != 0x09 || bytes[INTERFACE_DESC_OFFSET + 1] != 0x04 {
        return None;
    }
    let subclass = bytes[INTERFACE_DESC_OFFSET + 6];
    let protocol = bytes[INTERFACE_DESC_OFFSET + 7];
    Some((subclass, protocol))
}

#[test]
fn hid_passthrough_config_descriptor_reflects_inferred_boot_keyboard() {
    let collections = keyboard_collections();
    let report_descriptor = synthesize_report_descriptor(&collections).unwrap();
    let (interface_subclass, interface_protocol) =
        infer_boot_interface_subclass_protocol(&collections)
            .map(|(s, p)| (Some(s), Some(p)))
            .unwrap_or((None, None));

    let mut dev = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "WebHID".to_string(),
        "Keyboard".to_string(),
        None,
        report_descriptor,
        false,
        None,
        interface_subclass,
        interface_protocol,
    );

    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 256,
        },
        None,
    );
    let ControlResponse::Data(bytes) = resp else {
        panic!("expected config descriptor bytes, got {resp:?}");
    };
    assert_eq!(
        parse_interface_descriptor_fields(&bytes),
        Some((0x01, 0x01))
    );
}

fn sample_report_descriptor_output_with_id() -> Vec<u8> {
    vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x00, // Usage (Undefined)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x02, // Report ID (2)
        0x09, 0x00, // Usage (Undefined)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x91, 0x02, // Output (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

fn control_out_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(matches!(
        dev.handle_in(0, 0),
        UsbInResult::Data(data) if data.is_empty()
    ));
}

#[test]
fn set_report_enqueues_output_report() {
    let report_desc = sample_report_descriptor_output_with_id();

    let handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc,
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(handle.clone()));

    // Typical enumeration flow configures the device before class requests.
    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(handle.configured());

    // SET_REPORT (Output, report ID 2). Some stacks include the report-id prefix even though
    // wValue already carries it; ensure we strip it and enqueue only the payload.
    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x09,
            w_value: (2u16 << 8) | 2u16,
            w_index: 0,
            w_length: 3,
        }),
        UsbOutResult::Ack
    );
    assert!(matches!(
        dev.handle_out(0, &[2, 0xAA, 0xBB]),
        UsbOutResult::Ack
    ));

    assert!(matches!(
        dev.handle_in(0, 0),
        UsbInResult::Data(data) if data.is_empty()
    ));

    let got = handle
        .pop_output_report()
        .expect("expected queued output report");
    assert_eq!(
        got,
        UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 2,
            data: vec![0xAA, 0xBB],
        }
    );
}
