use aero_usb::hid::passthrough::{UsbHidPassthrough, UsbHidPassthroughOutputReport};
use aero_usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};
use aero_usb::usb::{SetupPacket, UsbDevice};

fn fixture_mouse_collections() -> Vec<HidCollectionInfo> {
    let json = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    ));
    serde_json::from_str(json).expect("fixture JSON should deserialize")
}

#[test]
fn webhid_fixture_synthesizes_non_empty_report_descriptor() {
    let collections = fixture_mouse_collections();
    let desc = synthesize_report_descriptor(&collections).expect("report descriptor synthesis should succeed");
    assert!(!desc.is_empty(), "expected a non-empty report descriptor");
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
    assert!(matches!(dev.handle_in(0, &mut buf), aero_usb::usb::UsbHandshake::Ack { .. }));
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
    assert!(matches!(dev.handle_in(0, &mut buf), aero_usb::usb::UsbHandshake::Ack { .. }));

    let got = dev.pop_output_report().expect("expected queued output report");
    assert_eq!(
        got,
        UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 2,
            data: vec![0xAA, 0xBB],
        }
    );
}
