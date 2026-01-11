use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::hid::{GamepadReport, UsbHidCompositeInput, UsbHidGamepad, UsbHidKeyboard, UsbHidMouse};
use aero_usb::usb::{SetupPacket, UsbDevice, UsbHandshake};

fn control_no_data<D: UsbDevice>(dev: &mut D, setup: SetupPacket) {
    dev.handle_setup(setup);
    let mut zlp: [u8; 0] = [];
    assert!(
        matches!(dev.handle_in(0, &mut zlp), UsbHandshake::Ack { .. }),
        "expected ACK for status stage"
    );
}

fn control_in<D: UsbDevice>(dev: &mut D, setup: SetupPacket, expected_len: usize) -> Vec<u8> {
    dev.handle_setup(setup);
    let mut buf = vec![0u8; expected_len];
    let got = match dev.handle_in(0, &mut buf) {
        UsbHandshake::Ack { bytes } => bytes,
        other => panic!("expected ACK for control IN data stage, got {other:?}"),
    };
    buf.truncate(got);

    // Status stage for control-IN is an OUT ZLP.
    assert!(
        matches!(dev.handle_out(0, &[]), UsbHandshake::Ack { .. }),
        "expected ACK for control-IN status stage"
    );
    buf
}

fn control_out_data<D: UsbDevice>(dev: &mut D, setup: SetupPacket, data: &[u8]) {
    dev.handle_setup(setup);
    assert!(
        matches!(
            dev.handle_out(0, data),
            UsbHandshake::Ack { bytes } if bytes == data.len()
        ),
        "expected ACK for control OUT data stage"
    );

    // Status stage for control-OUT is an IN ZLP.
    let mut zlp: [u8; 0] = [];
    assert!(
        matches!(dev.handle_in(0, &mut zlp), UsbHandshake::Ack { bytes: 0 }),
        "expected ACK for control-OUT status stage"
    );
}

#[test]
fn hid_keyboard_snapshot_roundtrip_preserves_leds_and_pending_reports() {
    let mut kb = UsbHidKeyboard::new();

    control_no_data(
        &mut kb,
        SetupPacket {
            request_type: 0x00,
            request: 0x05, // SET_ADDRESS
            value: 5,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut kb,
        SetupPacket {
            request_type: 0x00,
            request: 0x09, // SET_CONFIGURATION
            value: 1,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut kb,
        SetupPacket {
            request_type: 0x00,
            request: 0x03, // SET_FEATURE
            value: 1,      // DEVICE_REMOTE_WAKEUP
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut kb,
        SetupPacket {
            request_type: 0x21,
            request: 0x0b, // SET_PROTOCOL
            value: 0,      // boot protocol
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut kb,
        SetupPacket {
            request_type: 0x21,
            request: 0x0a, // SET_IDLE
            value: 5u16 << 8,
            index: 0,
            length: 0,
        },
    );

    // SET_REPORT(Output) to set keyboard LEDs.
    control_out_data(
        &mut kb,
        SetupPacket {
            request_type: 0x21,
            request: 0x09,
            value: 2u16 << 8, // Output report, ID 0
            index: 0,
            length: 1,
        },
        &[0x05],
    );

    // Queue two interrupt reports.
    kb.key_event(0x04, true); // 'a'
    kb.key_event(0x04, false);

    let snap = kb.save_state();

    let mut restored = UsbHidKeyboard::new();
    restored.load_state(&snap).unwrap();

    assert_eq!(restored.address(), 5);

    let status = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0x80,
            request: 0x00, // GET_STATUS
            value: 0,
            index: 0,
            length: 2,
        },
        2,
    );
    assert_eq!(status, [0x02, 0x00]);

    let protocol = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x03, // GET_PROTOCOL
            value: 0,
            index: 0,
            length: 1,
        },
        1,
    );
    assert_eq!(protocol, [0]);

    let idle = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x02, // GET_IDLE
            value: 0,
            index: 0,
            length: 1,
        },
        1,
    );
    assert_eq!(idle, [5]);

    let leds = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x01, // GET_REPORT
            value: 2u16 << 8, // Output report
            index: 0,
            length: 1,
        },
        1,
    );
    assert_eq!(leds, [0x05]);

    let mut buf = [0u8; 8];
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Ack { bytes: 8 });
    assert_eq!(buf, [0, 0, 0x04, 0, 0, 0, 0, 0]);
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Ack { bytes: 8 });
    assert_eq!(buf, [0; 8]);
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Nak);
}

#[test]
fn hid_mouse_snapshot_roundtrip_preserves_boot_protocol_and_reports() {
    let mut mouse = UsbHidMouse::new();

    control_no_data(
        &mut mouse,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 6,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut mouse,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut mouse,
        SetupPacket {
            request_type: 0x21,
            request: 0x0b, // SET_PROTOCOL
            value: 0,      // boot protocol
            index: 0,
            length: 0,
        },
    );

    mouse.movement(10, -5);

    let snap = mouse.save_state();

    let mut restored = UsbHidMouse::new();
    restored.load_state(&snap).unwrap();
    assert_eq!(restored.address(), 6);

    let mut buf = [0u8; 4];
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Ack { bytes: 3 });
    assert_eq!(&buf[..3], [0x00, 10, 251]);
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Nak);
}

#[test]
fn hid_gamepad_snapshot_roundtrip_preserves_report_queue() {
    let mut gp = UsbHidGamepad::new();

    control_no_data(
        &mut gp,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 7,
            index: 0,
            length: 0,
        },
    );
    control_no_data(
        &mut gp,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
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

    let snap = gp.save_state();

    let mut restored = UsbHidGamepad::new();
    restored.load_state(&snap).unwrap();
    assert_eq!(restored.address(), 7);

    // GET_REPORT(Input) returns current report (after button_event).
    let report = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x01,
            value: 1u16 << 8, // Input report, ID 0
            index: 0,
            length: 8,
        },
        8,
    );
    assert_eq!(report, [0x35, 0x12, 0x03, 10, 246, 5, 251, 0]);

    let mut buf = [0u8; 8];
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Ack { bytes: 8 });
    assert_eq!(buf, [0x34, 0x12, 0x03, 10, 246, 5, 251, 0]);
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Ack { bytes: 8 });
    assert_eq!(buf, [0x35, 0x12, 0x03, 10, 246, 5, 251, 0]);
    assert_eq!(restored.handle_in(1, &mut buf), UsbHandshake::Nak);
}

#[test]
fn hid_composite_snapshot_roundtrip_preserves_multiple_queues() {
    let mut dev = UsbHidCompositeInput::new();

    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 8,
            index: 0,
            length: 0,
        },
    );
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
    // Set mouse interface protocol to boot so the interrupt report is 3 bytes.
    control_no_data(
        &mut dev,
        SetupPacket {
            request_type: 0x21,
            request: 0x0b,
            value: 0,
            index: 1,
            length: 0,
        },
    );
    // Set keyboard LEDs (interface 0).
    control_out_data(
        &mut dev,
        SetupPacket {
            request_type: 0x21,
            request: 0x09,
            value: 2u16 << 8,
            index: 0,
            length: 1,
        },
        &[0x0A],
    );

    dev.key_event(0x04, true);
    dev.mouse_movement(10, -5);
    dev.gamepad_button_event(1, true);

    let snap = dev.save_state();

    let mut restored = UsbHidCompositeInput::new();
    restored.load_state(&snap).unwrap();
    assert_eq!(restored.address(), 8);

    let leds = control_in(
        &mut restored,
        SetupPacket {
            request_type: 0xA1,
            request: 0x01,
            value: 2u16 << 8,
            index: 0,
            length: 1,
        },
        1,
    );
    assert_eq!(leds, [0x0A]);

    let mut kb = [0u8; 8];
    assert_eq!(restored.handle_in(1, &mut kb), UsbHandshake::Ack { bytes: 8 });
    assert_eq!(kb, [0, 0, 0x04, 0, 0, 0, 0, 0]);

    let mut mouse_buf = [0u8; 4];
    assert_eq!(
        restored.handle_in(2, &mut mouse_buf),
        UsbHandshake::Ack { bytes: 3 }
    );
    assert_eq!(&mouse_buf[..3], [0x00, 10, 251]);

    let mut gp = [0u8; 8];
    assert_eq!(restored.handle_in(3, &mut gp), UsbHandshake::Ack { bytes: 8 });
    assert_eq!(gp, [1, 0, 8, 0, 0, 0, 0, 0]);
}

