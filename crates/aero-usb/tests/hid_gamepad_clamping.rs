use aero_usb::hid::{GamepadReport, UsbCompositeHidInputHandle, UsbHidGamepad};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

#[test]
fn hid_gamepad_set_hat_and_axes_clamp_to_descriptor_ranges() {
    let mut pad = UsbHidGamepad::new();

    // Hat switch: valid values are 0..=7, with 8 used as the neutral/null state.
    pad.set_hat(Some(7));
    assert_eq!(pad.current_input_report().hat, 7);
    pad.set_hat(Some(8));
    assert_eq!(pad.current_input_report().hat, 8);
    pad.set_hat(Some(99));
    assert_eq!(pad.current_input_report().hat, 8);
    pad.set_hat(None);
    assert_eq!(pad.current_input_report().hat, 8);

    // Axes: logical range is -127..=127 (HID logical minimum/maximum).
    pad.set_axes(-128, 127, -128, 127);
    let r = pad.current_input_report();
    assert_eq!(r.x, -127);
    assert_eq!(r.y, 127);
    assert_eq!(r.rx, -127);
    assert_eq!(r.ry, 127);

    // `set_report` should apply the same clamping semantics.
    pad.set_report(GamepadReport {
        buttons: 0,
        hat: 255,
        x: -128,
        y: 0,
        rx: 0,
        ry: -128,
    });
    let r = pad.current_input_report();
    assert_eq!(r.hat, 8);
    assert_eq!(r.x, -127);
    assert_eq!(r.y, 0);
    assert_eq!(r.rx, 0);
    assert_eq!(r.ry, -127);
}

fn composite_gamepad_get_report_bytes(dev: &mut UsbCompositeHidInputHandle) -> [u8; 8] {
    // Request the gamepad input report from interface 2 (see docs/usb-hid-gamepad.md).
    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0xa1,
            b_request: 0x01, // HID_REQUEST_GET_REPORT
            w_value: 0x0100, // Input report, report ID 0
            w_index: 2,      // gamepad interface
            w_length: 8,
        },
        None,
    );
    let ControlResponse::Data(data) = resp else {
        panic!("expected GET_REPORT to return Data, got {resp:?}");
    };
    data.as_slice().try_into().expect("expected 8-byte report")
}

#[test]
fn hid_composite_gamepad_set_hat_and_axes_clamp_to_descriptor_ranges() {
    let mut dev = UsbCompositeHidInputHandle::new();

    dev.gamepad_set_hat(Some(99));
    let bytes = composite_gamepad_get_report_bytes(&mut dev);
    assert_eq!(bytes[2] & 0x0f, 8);

    dev.gamepad_set_axes(-128, 127, -128, 127);
    let bytes = composite_gamepad_get_report_bytes(&mut dev);
    assert_eq!(bytes[3], (-127i8) as u8);
    assert_eq!(bytes[4], 127u8);
    assert_eq!(bytes[5], (-127i8) as u8);
    assert_eq!(bytes[6], 127u8);

    dev.gamepad_set_report(GamepadReport {
        buttons: 0,
        hat: 255,
        x: -128,
        y: 0,
        rx: 0,
        ry: -128,
    });
    let bytes = composite_gamepad_get_report_bytes(&mut dev);
    assert_eq!(bytes[2] & 0x0f, 8);
    assert_eq!(bytes[3], (-127i8) as u8);
    assert_eq!(bytes[4], 0u8);
    assert_eq!(bytes[5], 0u8);
    assert_eq!(bytes[6], (-127i8) as u8);
}
