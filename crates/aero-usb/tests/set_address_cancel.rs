use aero_usb::hid::passthrough::UsbHidPassthrough;
use aero_usb::hid::UsbHidKeyboard;
use aero_usb::hub::UsbHubDevice;
use aero_usb::usb::{SetupPacket, UsbDevice, UsbHandshake};

fn complete_status_in(dev: &mut impl UsbDevice) -> UsbHandshake {
    let mut buf = [0u8; 0];
    dev.handle_in(0, &mut buf)
}

fn control_in(dev: &mut impl UsbDevice, setup: SetupPacket) -> Vec<u8> {
    dev.handle_setup(setup);

    let mut out = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        match dev.handle_in(0, &mut buf) {
            UsbHandshake::Ack { bytes } => {
                out.extend_from_slice(&buf[..bytes]);
                if bytes < buf.len() {
                    break;
                }
            }
            UsbHandshake::Nak => break,
            UsbHandshake::Stall | UsbHandshake::Timeout => panic!("expected control IN data"),
        }
    }

    // Status stage (OUT ZLP).
    assert!(matches!(dev.handle_out(0, &[]), UsbHandshake::Ack { .. }));
    out
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

#[test]
fn invalid_set_address_nonzero_windex_stalls_hub() {
    let mut dev = UsbHubDevice::new();

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
fn new_setup_aborts_pending_set_address_hub() {
    let mut dev = UsbHubDevice::new();

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
fn new_setup_aborts_pending_set_configuration_keyboard() {
    let mut dev = UsbHidKeyboard::new();

    // Start SET_CONFIGURATION(1) but do not complete the status stage.
    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 0,
        length: 0,
    });

    // New SETUP aborts the pending configuration change.
    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x80,
                request: 0x08, // GET_CONFIGURATION
                value: 0,
                index: 0,
                length: 1,
            }
        ),
        vec![0]
    );
    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x80,
                request: 0x08, // GET_CONFIGURATION
                value: 0,
                index: 0,
                length: 1,
            }
        ),
        vec![0]
    );
}

#[test]
fn new_setup_aborts_pending_set_configuration_passthrough() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 0,
        length: 0,
    });

    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x80,
                request: 0x08, // GET_CONFIGURATION
                value: 0,
                index: 0,
                length: 1,
            }
        ),
        vec![0]
    );
    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x80,
                request: 0x08, // GET_CONFIGURATION
                value: 0,
                index: 0,
                length: 1,
            }
        ),
        vec![0]
    );
    assert!(!dev.configured());
}

#[test]
fn new_setup_aborts_pending_set_configuration_hub() {
    let mut dev = UsbHubDevice::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 0,
        length: 0,
    });

    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x80,
                request: 0x08, // GET_CONFIGURATION
                value: 0,
                index: 0,
                length: 1,
            }
        ),
        vec![0]
    );
    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x80,
                request: 0x08, // GET_CONFIGURATION
                value: 0,
                index: 0,
                length: 1,
            }
        ),
        vec![0]
    );
}
