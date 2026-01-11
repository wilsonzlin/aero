use aero_usb::device::{AttachedUsbDevice, UsbOutResult};
use aero_usb::hid::UsbHidKeyboard;
use aero_usb::hub::UsbHubDevice;
use aero_usb::{SetupPacket, UsbInResult};

fn status_in(dev: &mut AttachedUsbDevice) -> UsbInResult {
    dev.handle_in(0, 0)
}

fn control_in(dev: &mut AttachedUsbDevice, setup: SetupPacket) -> Vec<u8> {
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

    // Status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    out
}

fn setup_set_address(addr: u16, w_index: u16) -> SetupPacket {
    SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x05, // SET_ADDRESS
        w_value: addr,
        w_index,
        w_length: 0,
    }
}

fn setup_set_configuration(cfg: u16, w_index: u16) -> SetupPacket {
    SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09, // SET_CONFIGURATION
        w_value: cfg,
        w_index,
        w_length: 0,
    }
}

fn setup_get_configuration() -> SetupPacket {
    SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x08, // GET_CONFIGURATION
        w_value: 0,
        w_index: 0,
        w_length: 1,
    }
}

#[test]
fn invalid_set_address_nonzero_windex_stalls_keyboard() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHidKeyboard::new()));
    assert_eq!(
        dev.handle_setup(setup_set_address(1, 1)),
        UsbOutResult::Stall
    );
    assert_eq!(dev.address(), 0);
}

#[test]
fn new_setup_aborts_pending_set_address_keyboard() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHidKeyboard::new()));

    assert_eq!(dev.handle_setup(setup_set_address(5, 0)), UsbOutResult::Ack);
    assert_eq!(dev.address(), 0, "SET_ADDRESS applies after status stage");

    // Abort the SET_ADDRESS request before the status stage is executed.
    assert_eq!(
        dev.handle_setup(setup_set_configuration(1, 0)),
        UsbOutResult::Ack
    );
    assert!(matches!(
        status_in(&mut dev),
        UsbInResult::Data(data) if data.is_empty()
    ));
    assert_eq!(dev.address(), 0);
}

#[test]
fn set_configuration_applies_on_status_stage_keyboard() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHidKeyboard::new()));
    assert_eq!(
        dev.handle_setup(setup_set_configuration(1, 0)),
        UsbOutResult::Ack
    );

    // Complete the status stage (IN ZLP), which is when `SET_CONFIGURATION` should take effect.
    assert!(matches!(
        status_in(&mut dev),
        UsbInResult::Data(data) if data.is_empty()
    ));

    assert_eq!(control_in(&mut dev, setup_get_configuration()), vec![1]);
}

#[test]
fn new_setup_aborts_pending_set_configuration_keyboard() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHidKeyboard::new()));

    assert_eq!(
        dev.handle_setup(setup_set_configuration(1, 0)),
        UsbOutResult::Ack
    );

    // Abort the SET_CONFIGURATION request before the status stage is executed.
    assert_eq!(control_in(&mut dev, setup_get_configuration()), vec![0]);
    assert_eq!(control_in(&mut dev, setup_get_configuration()), vec![0]);
}

#[test]
fn invalid_set_address_nonzero_windex_stalls_hub() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHubDevice::new()));
    assert_eq!(
        dev.handle_setup(setup_set_address(1, 1)),
        UsbOutResult::Stall
    );
    assert_eq!(dev.address(), 0);
}

#[test]
fn new_setup_aborts_pending_set_address_hub() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHubDevice::new()));

    assert_eq!(dev.handle_setup(setup_set_address(5, 0)), UsbOutResult::Ack);
    assert_eq!(dev.address(), 0, "SET_ADDRESS applies after status stage");

    assert_eq!(
        dev.handle_setup(setup_set_configuration(1, 0)),
        UsbOutResult::Ack
    );
    assert!(matches!(
        status_in(&mut dev),
        UsbInResult::Data(data) if data.is_empty()
    ));
    assert_eq!(dev.address(), 0);
}

#[test]
fn set_configuration_applies_on_status_stage_hub() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHubDevice::new()));
    assert_eq!(
        dev.handle_setup(setup_set_configuration(1, 0)),
        UsbOutResult::Ack
    );
    assert!(matches!(
        status_in(&mut dev),
        UsbInResult::Data(data) if data.is_empty()
    ));
    assert_eq!(control_in(&mut dev, setup_get_configuration()), vec![1]);
}

#[test]
fn new_setup_aborts_pending_set_configuration_hub() {
    let mut dev = AttachedUsbDevice::new(Box::new(UsbHubDevice::new()));
    assert_eq!(
        dev.handle_setup(setup_set_configuration(1, 0)),
        UsbOutResult::Ack
    );
    assert_eq!(control_in(&mut dev, setup_get_configuration()), vec![0]);
    assert_eq!(control_in(&mut dev, setup_get_configuration()), vec![0]);
}
