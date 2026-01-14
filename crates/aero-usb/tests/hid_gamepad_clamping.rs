use aero_usb::hid::{GamepadReport, UsbHidGamepad};

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

