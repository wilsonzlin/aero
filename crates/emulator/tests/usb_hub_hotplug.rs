use emulator::io::usb::core::{UsbInResult, UsbOutResult};
use emulator::io::usb::hid::keyboard::UsbHidKeyboardHandle;
use emulator::io::usb::hub::{RootHub, UsbHubDevice};
use emulator::io::usb::{ControlResponse, SetupPacket};

const USB_REQUEST_GET_STATUS: u8 = 0x00;
const USB_REQUEST_CLEAR_FEATURE: u8 = 0x01;
const USB_REQUEST_SET_FEATURE: u8 = 0x03;
const USB_REQUEST_SET_ADDRESS: u8 = 0x05;

const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;
const HUB_PORT_FEATURE_C_PORT_CONNECTION: u16 = 16;

fn port_change_bits(resp: ControlResponse) -> u16 {
    let ControlResponse::Data(data) = resp else {
        panic!("expected GET_STATUS to return data");
    };
    assert_eq!(data.len(), 4);
    u16::from_le_bytes([data[2], data[3]])
}

#[test]
fn usb_hub_hotplug_attach_detach_at_path() {
    let mut root = RootHub::new();

    // Root port 0 has an external hub.
    root.attach(0, Box::new(UsbHubDevice::new()));
    root.force_enable_for_tests(0);

    // Enumerate the hub itself: address 0 -> address 1.
    {
        let dev = root
            .device_mut_for_address(0)
            .expect("hub should be visible at address 0");
        let setup = SetupPacket {
            bm_request_type: 0x00,
            b_request: USB_REQUEST_SET_ADDRESS,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
        assert!(matches!(dev.handle_in(0, 0), UsbInResult::Data(d) if d.is_empty()));
        assert_eq!(dev.address(), 1);
    }

    // Hotplug a keyboard behind the *already-attached and boxed* hub at downstream port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    root.attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach_at_path should succeed");

    // Prove the downstream device is unreachable until the hub port is powered + reset.
    assert!(root.device_mut_for_address(0).is_none());

    {
        let hub_dev = root
            .device_mut_for_address(1)
            .expect("hub should be visible at address 1");
        // SET_FEATURE(PORT_POWER), port 1.
        assert_eq!(
            hub_dev.model_mut().handle_control_request(
                SetupPacket {
                    bm_request_type: 0x23,
                    b_request: USB_REQUEST_SET_FEATURE,
                    w_value: HUB_PORT_FEATURE_POWER,
                    w_index: 1,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
        // SET_FEATURE(PORT_RESET), port 1.
        assert_eq!(
            hub_dev.model_mut().handle_control_request(
                SetupPacket {
                    bm_request_type: 0x23,
                    b_request: USB_REQUEST_SET_FEATURE,
                    w_value: HUB_PORT_FEATURE_RESET,
                    w_index: 1,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
    }

    for _ in 0..50 {
        root.tick_1ms();
    }

    {
        let dev = root
            .device_mut_for_address(0)
            .expect("downstream device should now be reachable at address 0");
        let desc = dev.model_mut().get_device_descriptor();
        // Hub uses bDeviceClass=0x09; keyboard uses 0x00.
        assert_eq!(desc.get(4).copied(), Some(0x00));
    }

    // Clear connection change so we can prove detach toggles it.
    {
        let hub_dev = root
            .device_mut_for_address(1)
            .expect("hub should still be visible at address 1");
        assert_eq!(
            hub_dev.model_mut().handle_control_request(
                SetupPacket {
                    bm_request_type: 0x23,
                    b_request: USB_REQUEST_CLEAR_FEATURE,
                    w_value: HUB_PORT_FEATURE_C_PORT_CONNECTION,
                    w_index: 1,
                    w_length: 0,
                },
                None,
            ),
            ControlResponse::Ack
        );
        let change_bits = port_change_bits(hub_dev.model_mut().handle_control_request(
            SetupPacket {
                bm_request_type: 0xa3,
                b_request: USB_REQUEST_GET_STATUS,
                w_value: 0,
                w_index: 1,
                w_length: 4,
            },
            None,
        ));
        assert_eq!(change_bits & 0x0001, 0);
    }

    root.detach_at_path(&[0, 1])
        .expect("detach_at_path should succeed");

    assert!(root.device_mut_for_address(0).is_none());

    {
        let hub_dev = root
            .device_mut_for_address(1)
            .expect("hub should still be visible at address 1");
        let change_bits = port_change_bits(hub_dev.model_mut().handle_control_request(
            SetupPacket {
                bm_request_type: 0xa3,
                b_request: USB_REQUEST_GET_STATUS,
                w_value: 0,
                w_index: 1,
                w_length: 4,
            },
            None,
        ));
        assert_ne!(change_bits & 0x0001, 0);
    }
}

