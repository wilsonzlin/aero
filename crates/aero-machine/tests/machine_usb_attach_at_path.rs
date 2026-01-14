#![cfg(not(target_arch = "wasm32"))]

use std::any::Any;

use aero_machine::{Machine, MachineConfig};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::{SetupPacket, UsbInResult, UsbOutResult};

fn set_address(dev: &mut AttachedUsbDevice, address: u8) {
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x05, // SET_ADDRESS
        w_value: address as u16,
        w_index: 0,
        w_length: 0,
    };

    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
    assert_eq!(dev.address(), address);
}

fn hub_set_feature(dev: &mut AttachedUsbDevice, feature: u16, port: u16) {
    let setup = SetupPacket {
        bm_request_type: 0x23, // Class | Other | HostToDevice
        b_request: 0x03,       // SET_FEATURE
        w_value: feature,
        w_index: port,
        w_length: 0,
    };

    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
}

#[test]
fn machine_usb_attach_at_path_attaches_keyboard_behind_nested_hub() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test minimal/deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Attach a USB hub at UHCI root port 0.
    m.usb_attach_at_path(&[0], Box::new(UsbHubDevice::with_port_count(2)))
        .expect("attach hub at root port 0");

    // Bypass the guest reset/enable dance and give the hub a non-zero address so the root hub can
    // route through it.
    {
        let uhci = m.uhci().expect("uhci enabled");
        let mut uhci = uhci.borrow_mut();
        let root = uhci.controller_mut().hub_mut();
        root.force_enable_for_tests(0);

        let mut hub_dev = root
            .device_mut_for_address(0)
            .expect("hub should be reachable at address 0");
        set_address(&mut hub_dev, 1);
    }

    // Attach a USB HID keyboard behind hub port 1 (path: root port 0 -> hub port 1).
    let keyboard = UsbHidKeyboardHandle::new();
    m.usb_attach_at_path(&[0, 1], Box::new(keyboard))
        .expect("attach keyboard behind hub");

    let uhci = m.uhci().expect("uhci enabled");
    let mut uhci = uhci.borrow_mut();
    let root = uhci.controller_mut().hub_mut();

    // Enable hub port 1 so the keyboard becomes reachable by address.
    {
        let mut hub_dev = root
            .device_mut_for_address(1)
            .expect("hub should be reachable at address 1");
        // Hub port feature selectors from the USB 2.0 hub class spec.
        const HUB_PORT_FEATURE_POWER: u16 = 8;
        const HUB_PORT_FEATURE_RESET: u16 = 4;
        hub_set_feature(&mut hub_dev, HUB_PORT_FEATURE_POWER, 1);
        hub_set_feature(&mut hub_dev, HUB_PORT_FEATURE_RESET, 1);
    }
    for _ in 0..50 {
        root.tick_1ms();
    }

    {
        let mut kb_dev = root
            .device_mut_for_address(0)
            .expect("keyboard should be reachable at address 0 once hub port is enabled");
        set_address(&mut kb_dev, 5);
    }

    let kb_dev = root
        .device_mut_for_address(5)
        .expect("keyboard should be routable by address once enumerated");
    assert_eq!(kb_dev.address(), 5);
    assert!(
        (kb_dev.model() as &dyn Any).is::<UsbHidKeyboardHandle>(),
        "routed device should be the attached keyboard model"
    );
}
