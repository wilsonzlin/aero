use aero_usb::hid::UsbHidPassthroughHandle;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

fn sample_report_descriptor_feature_with_id() -> Vec<u8> {
    vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (0x01)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x01, // Report ID (1)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0xb1, 0x02, // Feature (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

#[test]
fn get_report_feature_naks_then_completes_and_does_not_duplicate_host_requests() {
    let report = sample_report_descriptor_feature_with_id();
    let mut dev = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".into(),
        "Product".into(),
        None,
        report,
        false,
        None,
        None,
        None,
    );

    let setup = SetupPacket {
        bm_request_type: 0xa1,       // DeviceToHost | Class | Interface
        b_request: 0x01,             // GET_REPORT
        w_value: (3u16 << 8) | 1u16, // Feature, report ID 1
        w_index: 0,
        w_length: 64,
    };

    // First GET_REPORT must NAK and enqueue a single host-side request.
    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak
    );

    // Polling again before the host has serviced the request must still NAK and must not enqueue
    // duplicates.
    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak
    );

    let req = dev
        .pop_feature_report_request()
        .expect("expected enqueued feature report request");
    assert_eq!(req.report_id, 1);
    assert!(
        dev.pop_feature_report_request().is_none(),
        "must not enqueue duplicate host requests"
    );

    // While the request is pending, further guest polls should NAK without generating more host
    // requests (even after the host has popped the queue entry).
    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    assert!(
        dev.pop_feature_report_request().is_none(),
        "must not enqueue while request is in-flight"
    );

    // Inject the host completion (payload bytes do not include the report ID prefix).
    assert!(
        dev.complete_feature_report_request(req.request_id, req.report_id, &[0xAA, 0xBB]),
        "completion must be accepted"
    );

    // Next poll should return the completed bytes, including the report-id prefix.
    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Data(vec![1, 0xAA, 0xBB])
    );
}
