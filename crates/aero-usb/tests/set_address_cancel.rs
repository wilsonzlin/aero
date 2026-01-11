use aero_usb::hid::passthrough::UsbHidPassthrough;
use aero_usb::hid::{UsbHidCompositeInput, UsbHidGamepad, UsbHidKeyboard, UsbHidMouse};
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
fn invalid_set_address_nonzero_windex_stalls_mouse() {
    let mut dev = UsbHidMouse::new();

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
fn new_setup_aborts_pending_set_address_mouse() {
    let mut dev = UsbHidMouse::new();

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
fn invalid_set_address_nonzero_windex_stalls_gamepad() {
    let mut dev = UsbHidGamepad::new();

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
fn new_setup_aborts_pending_set_address_gamepad() {
    let mut dev = UsbHidGamepad::new();

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
fn invalid_set_address_nonzero_windex_stalls_composite() {
    let mut dev = UsbHidCompositeInput::new();

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
fn new_setup_aborts_pending_set_address_composite() {
    let mut dev = UsbHidCompositeInput::new();

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

#[test]
fn new_setup_aborts_pending_set_configuration_mouse() {
    let mut dev = UsbHidMouse::new();

    // Start SET_CONFIGURATION(1) but do not complete the status stage.
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

#[test]
fn new_setup_aborts_pending_set_configuration_gamepad() {
    let mut dev = UsbHidGamepad::new();

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

#[test]
fn new_setup_aborts_pending_set_configuration_composite() {
    let mut dev = UsbHidCompositeInput::new();

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

#[test]
fn invalid_set_configuration_nonzero_windex_stalls_keyboard() {
    let mut dev = UsbHidKeyboard::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
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
fn invalid_set_configuration_nonzero_windex_stalls_passthrough() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
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
fn invalid_set_configuration_nonzero_windex_stalls_hub() {
    let mut dev = UsbHubDevice::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
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
fn invalid_set_configuration_nonzero_windex_stalls_mouse() {
    let mut dev = UsbHidMouse::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
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
fn invalid_set_configuration_nonzero_windex_stalls_gamepad() {
    let mut dev = UsbHidGamepad::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
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
fn invalid_set_configuration_nonzero_windex_stalls_composite() {
    let mut dev = UsbHidCompositeInput::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00,
        request: 0x09, // SET_CONFIGURATION
        value: 1,
        index: 1, // invalid
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
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
fn invalid_get_configuration_nonzero_wvalue_stalls_keyboard() {
    let mut dev = UsbHidKeyboard::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x80, // IN | Standard | Device
        request: 0x08,      // GET_CONFIGURATION
        value: 1,           // invalid (must be 0)
        index: 0,
        length: 1,
    });

    let mut buf = [0u8; 1];
    assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Stall);
}

#[test]
fn invalid_get_status_nonzero_wvalue_stalls_keyboard() {
    let mut dev = UsbHidKeyboard::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x80, // IN | Standard | Device
        request: 0x00,      // GET_STATUS
        value: 1,           // invalid (must be 0)
        index: 0,
        length: 2,
    });

    let mut buf = [0u8; 2];
    assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Stall);
}

#[test]
fn invalid_set_feature_remote_wakeup_nonzero_windex_stalls_keyboard() {
    let mut dev = UsbHidKeyboard::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x00, // OUT | Standard | Device
        request: 0x03,      // SET_FEATURE
        value: 1,           // DEVICE_REMOTE_WAKEUP
        index: 1,           // invalid (must be 0)
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
}

#[test]
fn invalid_get_configuration_nonzero_wvalue_stalls_passthrough() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x80, // IN | Standard | Device
        request: 0x08,      // GET_CONFIGURATION
        value: 1,           // invalid (must be 0)
        index: 0,
        length: 1,
    });

    let mut buf = [0u8; 1];
    assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Stall);
}

#[test]
fn invalid_get_configuration_nonzero_wvalue_stalls_hub() {
    let mut dev = UsbHubDevice::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x80, // IN | Standard | Device
        request: 0x08,      // GET_CONFIGURATION
        value: 1,           // invalid (must be 0)
        index: 0,
        length: 1,
    });

    let mut buf = [0u8; 1];
    assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Stall);
}

#[test]
fn invalid_get_status_nonzero_wvalue_stalls_passthrough() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x80, // IN | Standard | Device
        request: 0x00,      // GET_STATUS
        value: 1,           // invalid (must be 0)
        index: 0,
        length: 2,
    });

    let mut buf = [0u8; 2];
    assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Stall);
}

#[test]
fn invalid_set_feature_remote_wakeup_nonzero_windex_stalls_passthrough() {
    let mut dev = UsbHidPassthrough::default();

    dev.handle_setup(SetupPacket {
        request_type: 0x00, // OUT | Standard | Device
        request: 0x03,      // SET_FEATURE
        value: 1,           // DEVICE_REMOTE_WAKEUP
        index: 1,           // invalid (must be 0)
        length: 0,
    });

    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);
}

#[test]
fn composite_endpoint_feature_requires_windex_high_byte_zero() {
    let mut dev = UsbHidCompositeInput::new();

    // SET_FEATURE(ENDPOINT_HALT) with a nonzero high byte in wIndex is invalid and must STALL.
    dev.handle_setup(SetupPacket {
        request_type: 0x02, // OUT | Standard | Endpoint
        request: 0x03,      // SET_FEATURE
        value: 0,           // ENDPOINT_HALT
        index: 0x0181,      // invalid (endpoint address must be in low byte only)
        length: 0,
    });
    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);

    // Endpoint status should remain not-halted.
    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x82, // IN | Standard | Endpoint
                request: 0x00,      // GET_STATUS
                value: 0,
                index: 0x81,
                length: 2,
            }
        ),
        vec![0, 0]
    );
}

#[test]
fn hub_endpoint_feature_requires_windex_high_byte_zero() {
    let mut dev = UsbHubDevice::new();

    dev.handle_setup(SetupPacket {
        request_type: 0x02, // OUT | Standard | Endpoint
        request: 0x03,      // SET_FEATURE
        value: 0,           // ENDPOINT_HALT
        index: 0x0181,      // invalid
        length: 0,
    });
    assert_eq!(complete_status_in(&mut dev), UsbHandshake::Stall);

    assert_eq!(
        control_in(
            &mut dev,
            SetupPacket {
                request_type: 0x82, // IN | Standard | Endpoint
                request: 0x00,      // GET_STATUS
                value: 0,
                index: 0x81,
                length: 2,
            }
        ),
        vec![0, 0]
    );
}
