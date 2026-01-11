use aero_usb::device::{AttachedUsbDevice, UsbOutResult};
use aero_usb::hid::passthrough::{UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};
use aero_usb::hid::report_descriptor::parse_report_descriptor;
use aero_usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};
use aero_usb::{SetupPacket, UsbInResult};

fn fixture_mouse_collections() -> Vec<HidCollectionInfo> {
    let json = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    ));
    serde_json::from_str(json).expect("fixture JSON should deserialize")
}

#[test]
fn webhid_fixture_json_roundtrips_and_synthesizes_descriptor() {
    let collections = fixture_mouse_collections();

    // Lock down the JSON wire contract: serde -> JSON must roundtrip without dropping/renaming any
    // fields. This should match the output of `web/src/hid/webhid_normalize.ts`.
    let expected_json: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    )))
    .expect("fixture JSON should parse");
    let actual_json = serde_json::to_value(&collections).expect("fixture should serialize");
    assert_eq!(actual_json, expected_json);

    let desc = synthesize_report_descriptor(&collections)
        .expect("report descriptor synthesis should succeed");
    assert!(!desc.is_empty(), "expected a non-empty report descriptor");

    // The synthesized bytes must also be parseable by our HID descriptor parser.
    parse_report_descriptor(&desc).expect("synthesized report descriptor should parse");
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
