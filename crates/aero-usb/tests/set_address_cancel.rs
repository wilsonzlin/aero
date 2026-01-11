use aero_usb::hid::passthrough::UsbHidPassthrough;
use aero_usb::hid::UsbHidKeyboard;
use aero_usb::usb::{SetupPacket, UsbDevice, UsbHandshake};

fn complete_status_in(dev: &mut impl UsbDevice) -> UsbHandshake {
    let mut buf = [0u8; 0];
    dev.handle_in(0, &mut buf)
}

#[test]
fn invalid_set_address_nonzero_windex_stalls_keyboard() {
    let mut dev = UsbHidKeyboard::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x05, // SET_ADDRESS
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
    assert_eq!(dev.address(), 0);
}

#[test]
fn new_setup_aborts_pending_set_address_keyboard() {
    let mut dev = UsbHidKeyboard::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x05, // SET_ADDRESS
        value: 5,
        index: 0,
        length: 0,
    });

    // Abort the SET_ADDRESS request before the status stage is executed.
    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 0,
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Ack { bytes: 0 });
    assert_eq!(dev.address(), 0);
}

#[test]
fn invalid_set_address_nonzero_windex_stalls_passthrough() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x05, // SET_ADDRESS
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
    assert_eq!(dev.address(), 0);
}

#[test]
fn new_setup_aborts_pending_set_address_passthrough() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x05, // SET_ADDRESS
        value: 5,
        index: 0,
        length: 0,
    });

    // Abort the SET_ADDRESS request before the status stage is executed.
    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 0,
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Ack { bytes: 0 });
    assert_eq!(dev.address(), 0);
}
