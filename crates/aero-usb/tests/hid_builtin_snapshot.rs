use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::{
    GamepadReport, UsbCompositeHidInputHandle, UsbHidGamepadHandle, UsbHidKeyboardHandle,
    UsbHidMouseHandle,
};
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_in(dev: &mut AttachedUsbDevice, setup: SetupPacket, expected_len: usize) -> Vec<u8> {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let mut out = Vec::new();
    loop {
        match dev.handle_in(0, 64) {
            UsbInResult::Data(chunk) => {
                out.extend_from_slice(&chunk);
                if chunk.len() < 64 {
                    break;
                }
            }
            UsbInResult::Nak => continue,
            UsbInResult::Stall => panic!("unexpected STALL during control IN transfer"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control IN transfer"),
        }
    }

    // Status stage for control-IN is an OUT ZLP.
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);

    out.truncate(expected_len);
    out
}

fn control_out_data(dev: &mut AttachedUsbDevice, setup: SetupPacket, data: &[u8]) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert_eq!(dev.handle_out(0, data), UsbOutResult::Ack);

    // Status stage for control-OUT is an IN ZLP.
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(resp) if resp.is_empty()),
        "expected ACK for control-OUT status stage"
    );
}

#[test]
fn hid_keyboard_snapshot_roundtrip_preserves_leds_and_pending_reports() {
    let kb = UsbHidKeyboardHandle::new();
    let mut dev = AttachedUsbDevice::new(Box::new(kb.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 5,
            w_index: 0,
            w_length: 0,
        },
    );
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
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 1,      // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x0b, // SET_PROTOCOL
            w_value: 0,      // boot protocol
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x0a, // SET_IDLE
            w_value: 5u16 << 8,
            w_index: 0,
            w_length: 0,
        },
    );

    // SET_REPORT(Output) to set keyboard LEDs.
    control_out_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x09,
            w_value: 2u16 << 8, // Output report, ID 0
            w_index: 0,
            w_length: 1,
        },
        &[0x05],
    );

    // Queue two interrupt reports.
    kb.key_event(0x04, true); // 'a'
    kb.key_event(0x04, false);

    let dev_snap = dev.save_state();
    let model_snap = kb.save_state();

    let mut restored_model = UsbHidKeyboardHandle::new();
    restored_model.load_state(&model_snap).unwrap();
    let mut restored = AttachedUsbDevice::new(Box::new(restored_model.clone()));
    restored.load_state(&dev_snap).unwrap();

    assert_eq!(restored.address(), 5);

    let status = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x00, // GET_STATUS
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
        2,
    );
    assert_eq!(status, [0x02, 0x00]);

    let protocol = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x03, // GET_PROTOCOL
            w_value: 0,
            w_index: 0,
            w_length: 1,
        },
        1,
    );
    assert_eq!(protocol, [0]);

    let idle = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x02, // GET_IDLE
            w_value: 0,
            w_index: 0,
            w_length: 1,
        },
        1,
    );
    assert_eq!(idle, [5]);

    let leds = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x01,    // GET_REPORT
            w_value: 2u16 << 8, // Output report
            w_index: 0,
            w_length: 1,
        },
        1,
    );
    assert_eq!(leds, [0x05]);

    assert!(
        matches!(restored.handle_in(1, 8), UsbInResult::Data(data) if data == vec![0, 0, 0x04, 0, 0, 0, 0, 0])
    );
    assert!(matches!(restored.handle_in(1, 8), UsbInResult::Data(data) if data == vec![0; 8]));
    assert!(matches!(restored.handle_in(1, 8), UsbInResult::Nak));
}

#[test]
fn hid_mouse_snapshot_roundtrip_preserves_boot_protocol_and_reports() {
    let mouse = UsbHidMouseHandle::new();
    let mut dev = AttachedUsbDevice::new(Box::new(mouse.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 6,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x0b, // SET_PROTOCOL
            w_value: 0,      // boot protocol
            w_index: 0,
            w_length: 0,
        },
    );

    mouse.movement(10, -5);

    let dev_snap = dev.save_state();
    let model_snap = mouse.save_state();

    let mut restored_model = UsbHidMouseHandle::new();
    restored_model.load_state(&model_snap).unwrap();
    let mut restored = AttachedUsbDevice::new(Box::new(restored_model.clone()));
    restored.load_state(&dev_snap).unwrap();
    assert_eq!(restored.address(), 6);

    assert!(
        matches!(restored.handle_in(1, 5), UsbInResult::Data(data) if data == vec![0x00, 10, 251])
    );
    assert!(matches!(restored.handle_in(1, 5), UsbInResult::Nak));
}

#[test]
fn hid_gamepad_snapshot_roundtrip_preserves_report_queue() {
    let gp = UsbHidGamepadHandle::new();
    let mut dev = AttachedUsbDevice::new(Box::new(gp.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 7,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    gp.set_report(GamepadReport {
        buttons: 0x1234,
        hat: 3,
        x: 10,
        y: -10,
        rx: 5,
        ry: -5,
    });
    gp.button_event(1, true);

    let dev_snap = dev.save_state();
    let model_snap = gp.save_state();

    let mut restored_model = UsbHidGamepadHandle::new();
    restored_model.load_state(&model_snap).unwrap();
    let mut restored = AttachedUsbDevice::new(Box::new(restored_model.clone()));
    restored.load_state(&dev_snap).unwrap();
    assert_eq!(restored.address(), 7);

    // GET_REPORT(Input) returns current report (after button_event).
    let report = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x01,
            w_value: 1u16 << 8, // Input report, ID 0
            w_index: 0,
            w_length: 8,
        },
        8,
    );
    assert_eq!(report, [0x35, 0x12, 0x03, 10, 246, 5, 251, 0]);

    assert!(
        matches!(restored.handle_in(1, 8), UsbInResult::Data(data) if data == vec![0x34, 0x12, 0x03, 10, 246, 5, 251, 0])
    );
    assert!(
        matches!(restored.handle_in(1, 8), UsbInResult::Data(data) if data == vec![0x35, 0x12, 0x03, 10, 246, 5, 251, 0])
    );
    assert!(matches!(restored.handle_in(1, 8), UsbInResult::Nak));
}

#[test]
fn hid_composite_snapshot_roundtrip_preserves_multiple_queues() {
    let composite = UsbCompositeHidInputHandle::new();
    let mut dev = AttachedUsbDevice::new(Box::new(composite.clone()));

    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 8,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    // Set mouse interface protocol to boot so the interrupt report is 3 bytes.
    control_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x0b,
            w_value: 0,
            w_index: 1,
            w_length: 0,
        },
    );
    // Set keyboard LEDs (interface 0).
    control_out_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x21,
            b_request: 0x09,
            w_value: 2u16 << 8,
            w_index: 0,
            w_length: 1,
        },
        &[0x0A],
    );

    composite.key_event(0x04, true);
    composite.mouse_movement(10, -5);
    composite.gamepad_button_event(1, true);

    let dev_snap = dev.save_state();
    let model_snap = composite.save_state();

    let mut restored_model = UsbCompositeHidInputHandle::new();
    restored_model.load_state(&model_snap).unwrap();
    let mut restored = AttachedUsbDevice::new(Box::new(restored_model.clone()));
    restored.load_state(&dev_snap).unwrap();
    assert_eq!(restored.address(), 8);

    let leds = control_in(
        &mut restored,
        SetupPacket {
            bm_request_type: 0xA1,
            b_request: 0x01,
            w_value: 2u16 << 8,
            w_index: 0,
            w_length: 1,
        },
        1,
    );
    assert_eq!(leds, [0x0A]);

    assert!(
        matches!(restored.handle_in(1, 8), UsbInResult::Data(data) if data == vec![0, 0, 0x04, 0, 0, 0, 0, 0])
    );

    assert!(
        matches!(restored.handle_in(2, 5), UsbInResult::Data(data) if data == vec![0x00, 10, 251])
    );

    assert!(
        matches!(restored.handle_in(3, 8), UsbInResult::Data(data) if data == vec![1, 0, 8, 0, 0, 0, 0, 0])
    );
}
