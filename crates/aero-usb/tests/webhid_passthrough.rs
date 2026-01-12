use aero_usb::hid::passthrough::{UsbHidPassthrough, UsbHidPassthroughOutputReport};
use aero_usb::hid::report_descriptor::parse_report_descriptor;
use aero_usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};
use aero_usb::usb::{SetupPacket, UsbDevice, UsbHandshake};

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

fn control_no_data(dev: &mut UsbHidPassthrough, setup: SetupPacket) {
    dev.handle_setup(setup);
    let mut buf = [0u8; 0];
    assert!(matches!(
        dev.handle_in(0, &mut buf),
        aero_usb::usb::UsbHandshake::Ack { .. }
    ));
}

#[test]
fn set_report_enqueues_output_report() {
    let report_desc = sample_report_descriptor_output_with_id();

    let mut dev = UsbHidPassthrough::new(
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

    // Typical enumeration flow configures the device before class requests.
    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );
    assert!(dev.configured());

    // SET_REPORT (Output, report ID 2). Some stacks include the report-id prefix even though
    // wValue already carries it; ensure we strip it and enqueue only the payload.
    dev.handle_setup(SetupPacket {
        request_type: 0x21,
        request: 0x09,
        value: (2u16 << 8) | 2u16,
        index: 0,
        length: 3,
    });
    assert!(matches!(
        dev.handle_out(0, &[2, 0xAA, 0xBB]),
        aero_usb::usb::UsbHandshake::Ack { bytes: 3 }
    ));

    let mut buf = [0u8; 0];
    assert!(matches!(
        dev.handle_in(0, &mut buf),
        aero_usb::usb::UsbHandshake::Ack { .. }
    ));

    let got = dev
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

#[test]
fn invalid_set_address_stalls() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x05,
        value: 12,
        index: 1,
        length: 0,
    });

    let mut buf = [0u8; 0];
    assert!(matches!(
        dev.handle_in(0, &mut buf),
        aero_usb::usb::UsbHandshake::Stall
    ));
    assert_eq!(dev.address(), 0);
}

#[test]
fn aborted_set_address_does_not_apply_on_later_status_stage() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x05,
        value: 12,
        index: 0,
        length: 0,
    });

    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );

    assert_eq!(dev.address(), 0);
    assert!(dev.configured());
}

#[test]
fn set_address_applies_on_status_stage() {
    let mut dev = UsbHidPassthrough::default();

    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 12,
            index: 0,
            length: 0,
        },
    );

    assert_eq!(dev.address(), 12);
}

#[test]
fn string_descriptors_are_capped_to_u8_length_and_remain_valid_utf16() {
    let long_ascii = "A".repeat(512);
    let mut dev = UsbHidPassthrough::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        long_ascii,
        None,
        Vec::new(),
        false,
        None,
        None,
        None,
    );

    dev.handle_setup(SetupPacket {
        request_type: 0x80,
        request: 0x06, // GET_DESCRIPTOR
        value: 0x0302, // STRING descriptor, index 2 (iProduct)
        index: 0,
        length: 255,
    });

    let mut buf = [0u8; 512];
    let bytes = match dev.handle_in(0, &mut buf) {
        UsbHandshake::Ack { bytes } => bytes,
        other => panic!("expected ACK for string descriptor, got {other:?}"),
    };
    assert_eq!(bytes, 254, "string descriptor must be capped to 254 bytes");
    assert_eq!(buf[0] as usize, bytes, "bLength must match payload length");
    assert_eq!(buf[1], 0x03, "bDescriptorType must be STRING");

    // Ensure the UTF-16 payload decodes (no unpaired surrogates).
    let utf16: Vec<u16> = buf[2..bytes]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let decoded = String::from_utf16(&utf16).expect("string descriptor must be valid UTF-16");
    assert_eq!(
        decoded.len(),
        126,
        "expected 126 ASCII characters after truncation"
    );

    // Non-BMP case: ensure we don't truncate in the middle of a surrogate pair.
    let long_emoji = "😀".repeat(100);
    let mut dev2 = UsbHidPassthrough::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        long_emoji,
        None,
        Vec::new(),
        false,
        None,
        None,
        None,
    );
    dev2.handle_setup(SetupPacket {
        request_type: 0x80,
        request: 0x06,
        value: 0x0302,
        index: 0,
        length: 255,
    });
    let mut buf2 = [0u8; 512];
    let bytes2 = match dev2.handle_in(0, &mut buf2) {
        UsbHandshake::Ack { bytes } => bytes,
        other => panic!("expected ACK for string descriptor, got {other:?}"),
    };
    assert_eq!(bytes2, 254, "expected descriptor to fill the 254-byte cap");
    assert_eq!(buf2[0] as usize, bytes2);
    assert_eq!(buf2[1], 0x03);
    let utf16: Vec<u16> = buf2[2..bytes2]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let decoded = String::from_utf16(&utf16).expect("emoji string must be valid UTF-16");
    assert_eq!(decoded.chars().count(), 63, "expected 63 emoji characters");
}
