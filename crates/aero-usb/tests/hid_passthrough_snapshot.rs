use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotWriter};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::{UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_out_data(dev: &mut AttachedUsbDevice, setup: SetupPacket, data: &[u8]) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert_eq!(dev.handle_out(0, data), UsbOutResult::Ack);

    // Status stage for control-OUT is an IN ZLP. Asynchronous models may NAK until work completes.
    loop {
        match dev.handle_in(0, 0) {
            UsbInResult::Data(resp) => {
                assert!(resp.is_empty(), "expected ZLP for status stage");
                break;
            }
            UsbInResult::Nak => continue,
            UsbInResult::Stall => panic!("unexpected STALL during control OUT status stage"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control OUT status stage"),
        }
    }
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

fn sample_report_descriptor_input_2_bytes() -> Vec<u8> {
    vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (0x01)
        0xa1, 0x01, // Collection (Application)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

#[test]
fn hid_passthrough_snapshot_preserves_pending_input_reports() {
    let report_desc = sample_report_descriptor_input_2_bytes();
    let dev_handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc.clone(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(dev_handle.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    dev_handle.push_input_report(0, &[0xaa, 0xbb]);

    let snapshot = dev_handle.save_state();

    let mut restored_handle = UsbHidPassthroughHandle::new(
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
    restored_handle.load_state(&snapshot).unwrap();

    let mut restored = AttachedUsbDevice::new(Box::new(restored_handle));
    assert!(
        matches!(restored.handle_in(1, 64), UsbInResult::Data(data) if data == vec![0xaa, 0xbb]),
        "expected pending input report after restore"
    );
    assert!(matches!(restored.handle_in(1, 64), UsbInResult::Nak));
}

#[test]
fn hid_passthrough_snapshot_preserves_pending_output_reports() {
    let report_desc = sample_report_descriptor_output_with_id();
    let dev_handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc.clone(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(dev_handle.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // SET_REPORT(Output) should be queued for the host-side integration.
    control_out_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,       // HostToDevice | Class | Interface
            b_request: 0x09,             // SET_REPORT
            w_value: (2u16 << 8) | 2u16, // Output report, ID 2
            w_index: 0,
            w_length: 3, // report ID + payload
        },
        &[2, 0xde, 0xad],
    );

    let snapshot = dev_handle.save_state();

    let mut restored_handle = UsbHidPassthroughHandle::new(
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
    restored_handle.load_state(&snapshot).unwrap();

    assert_eq!(
        restored_handle.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 2,
            data: vec![0xde, 0xad],
        })
    );
    assert!(restored_handle.pop_output_report().is_none());
}

#[test]
fn hid_passthrough_unconfigured_restore_drops_pending_reports_on_set_configuration() {
    // TAG constants from `UsbHidPassthrough::load_state`.
    const TAG_CONFIGURATION: u16 = 2;

    let report_desc = sample_report_descriptor_input_2_bytes();
    let dev_handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_desc.clone(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(dev_handle.clone()));

    // The report queue is only active once configured.
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Queue a non-default report, then a default report so `last_input_reports` is default.
    dev_handle.push_input_report(0, &[0x11, 0x22]);
    dev_handle.push_input_report(0, &[0x00, 0x00]);

    let snapshot = dev_handle.save_state();

    // Simulate restoring a snapshot where the device is unconfigured but still has queued reports
    // (e.g. an older snapshot format without the configuration field).
    let r = SnapshotReader::parse(
        &snapshot,
        <UsbHidPassthroughHandle as IoSnapshot>::DEVICE_ID,
    )
    .unwrap();
    let header = r.header();
    let mut w = SnapshotWriter::new(header.device_id, header.device_version);
    for (tag, bytes) in r.iter_fields() {
        if tag == TAG_CONFIGURATION {
            w.field_u8(TAG_CONFIGURATION, 0);
        } else {
            w.field_bytes(tag, bytes.to_vec());
        }
    }
    let unconfigured_snapshot = w.finish();

    let mut restored_handle = UsbHidPassthroughHandle::new(
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
    restored_handle.load_state(&unconfigured_snapshot).unwrap();
    assert!(!restored_handle.configured());

    let mut restored = AttachedUsbDevice::new(Box::new(restored_handle.clone()));

    // Even though the snapshot restored a pending queue, the device is unconfigured so interrupt
    // IN must NAK.
    assert!(matches!(restored.handle_in(1, 64), UsbInResult::Nak));

    // Once configured, any reports that were persisted while unconfigured must be dropped to avoid
    // replaying stale events.
    control_no_data(
        &mut restored,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(restored_handle.configured());

    assert!(matches!(restored.handle_in(1, 64), UsbInResult::Nak));
}
