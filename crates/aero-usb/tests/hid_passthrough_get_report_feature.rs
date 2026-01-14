use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::device::{AttachedUsbDevice, UsbOutResult};
use aero_usb::hid::passthrough::UsbHidPassthroughHandle;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;
const HID_REQUEST_GET_REPORT: u8 = 0x01;

fn control_out_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(matches!(
        dev.handle_in(0, 0),
        UsbInResult::Data(data) if data.is_empty()
    ));
}

fn drain_control_in_data_stage(dev: &mut AttachedUsbDevice, max_packet: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        match dev.handle_in(0, max_packet) {
            UsbInResult::Data(chunk) => {
                out.extend_from_slice(&chunk);
                if chunk.len() < max_packet {
                    break;
                }
            }
            UsbInResult::Nak => continue,
            UsbInResult::Stall => panic!("unexpected STALL during control IN transfer"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control IN transfer"),
        }
    }

    // Status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    out
}

fn sample_feature_report_descriptor_with_id() -> Vec<u8> {
    // Single feature report:
    // - Report ID 7
    // - 3 bytes payload
    vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x00, // Usage (Undefined)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x07, // Report ID (7)
        0x09, 0x00, // Usage (Undefined)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x03, // Report Count (3)
        0xb1, 0x02, // Feature (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

#[test]
fn get_report_feature_naks_until_host_completion_and_includes_report_id_prefix() {
    let handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_feature_report_descriptor_with_id(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(handle.clone()));

    // Configure the device first (typical enumeration flow).
    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: USB_REQUEST_SET_CONFIGURATION,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(handle.configured());

    // GET_REPORT (Feature, report ID 7). Total report length is 1 (ID) + 3 payload bytes.
    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0xa1, // DeviceToHost | Class | Interface
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 7u16,
            w_index: 0,
            w_length: 4,
        }),
        UsbOutResult::Ack
    );

    // DATA stage should NAK until the host completes the proxied read.
    assert_eq!(dev.handle_in(0, 64), UsbInResult::Nak);

    let req = handle
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.report_id, 7);

    // Repeated polls should not enqueue duplicates.
    assert_eq!(dev.handle_in(0, 64), UsbInResult::Nak);
    assert!(handle.pop_feature_report_request().is_none());

    // Provide a payload shorter than the descriptor-defined length; it should be zero-padded.
    assert!(handle.complete_feature_report_request(req.request_id, req.report_id, &[0xAA, 0xBB]));

    let data = match dev.handle_in(0, 64) {
        UsbInResult::Data(data) => data,
        other => panic!("expected DATA after completion, got {other:?}"),
    };
    assert_eq!(data, vec![7, 0xAA, 0xBB, 0x00]);

    // Status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
}

#[test]
fn get_report_feature_unknown_report_id_is_capped() {
    let handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_feature_report_descriptor_with_id(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(handle.clone()));

    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: USB_REQUEST_SET_CONFIGURATION,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Request an unknown report ID 9 with a very large wLength; the model must not allocate that
    // much for the response.
    let huge_w_length: u16 = 0xffff;
    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0xa1,
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 9u16,
            w_index: 0,
            w_length: huge_w_length,
        }),
        UsbOutResult::Ack
    );
    assert_eq!(dev.handle_in(0, 64), UsbInResult::Nak);
    let req = handle
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.report_id, 9);

    // Provide an oversized payload; it should be capped to a small maximum.
    let oversized = vec![0x11u8; 10 * 1024];
    assert!(handle.complete_feature_report_request(req.request_id, req.report_id, &oversized));

    let data = drain_control_in_data_stage(&mut dev, 64);
    // Unknown report IDs are hard-capped to 4096 bytes of payload; with a non-zero report ID, the
    // device model prefixes it to match USB HID `GET_REPORT` semantics.
    assert_eq!(data.len(), 4097);
    assert_eq!(data[0], 9);
}

#[test]
fn cancel_control_transfer_clears_pending_feature_request() {
    let handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_feature_report_descriptor_with_id(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(handle.clone()));

    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: USB_REQUEST_SET_CONFIGURATION,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Start a GET_REPORT feature transfer (will NAK and queue a host request).
    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0xa1,
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 7u16,
            w_index: 0,
            w_length: 4,
        }),
        UsbOutResult::Ack
    );
    assert_eq!(dev.handle_in(0, 64), UsbInResult::Nak);

    let req = handle
        .pop_feature_report_request()
        .expect("expected queued feature report request");

    // Abort the control transfer by issuing a new SETUP.
    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0xa1,
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (1u16 << 8) | 0u16,
            w_index: 0,
            w_length: 0,
        }),
        UsbOutResult::Ack
    );

    // Completing the stale request should be ignored.
    assert!(!handle.complete_feature_report_request(req.request_id, req.report_id, &[1, 2, 3]));

    // A new GET_REPORT should enqueue a fresh request ID.
    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0xa1,
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 7u16,
            w_index: 0,
            w_length: 4,
        }),
        UsbOutResult::Ack
    );
    assert_eq!(dev.handle_in(0, 64), UsbInResult::Nak);
    let req2 = handle
        .pop_feature_report_request()
        .expect("expected re-queued feature report request after cancel");
    assert_ne!(req2.request_id, req.request_id);
}

#[test]
fn feature_report_completion_error_stalls() {
    let handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_feature_report_descriptor_with_id(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(handle.clone()));

    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: USB_REQUEST_SET_CONFIGURATION,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    assert_eq!(
        dev.handle_setup(SetupPacket {
            bm_request_type: 0xa1,
            b_request: HID_REQUEST_GET_REPORT,
            w_value: (3u16 << 8) | 7u16,
            w_index: 0,
            w_length: 4,
        }),
        UsbOutResult::Ack
    );
    assert_eq!(dev.handle_in(0, 64), UsbInResult::Nak);

    let req = handle
        .pop_feature_report_request()
        .expect("expected queued feature report request");

    assert!(handle.fail_feature_report_request(req.request_id, req.report_id));

    assert_eq!(dev.handle_in(0, 64), UsbInResult::Timeout);
}

#[test]
fn snapshot_restore_requeues_inflight_feature_report_request() {
    let report = sample_feature_report_descriptor_with_id();
    let mut dev = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report.clone(),
        false,
        None,
        None,
        None,
    );

    let setup = SetupPacket {
        bm_request_type: 0xa1,
        b_request: HID_REQUEST_GET_REPORT,
        w_value: (3u16 << 8) | 7u16,
        w_index: 0,
        w_length: 4,
    };

    assert_eq!(dev.handle_control_request(setup, None), ControlResponse::Nak);
    let req = dev
        .pop_feature_report_request()
        .expect("expected queued feature report request");

    // Snapshot after the host has drained the request queue but before it has completed the host
    // read.
    let snapshot = dev.save_state();

    let mut restored = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report,
        false,
        None,
        None,
        None,
    );
    restored
        .load_state(&snapshot)
        .expect("snapshot restore should succeed");

    // After restore, the in-flight request should be re-queued so the host runtime can discover it
    // again.
    assert_eq!(
        restored.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    let req2 = restored
        .pop_feature_report_request()
        .expect("expected re-queued request after snapshot restore");
    assert_eq!(req2, req);

    assert!(restored.complete_feature_report_request(
        req2.request_id,
        req2.report_id,
        &[0xAA, 0xBB, 0xCC]
    ));
    assert_eq!(
        restored.handle_control_request(setup, None),
        ControlResponse::Data(vec![7, 0xAA, 0xBB, 0xCC])
    );
}
