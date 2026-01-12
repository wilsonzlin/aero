use emulator::io::usb::core::{UsbInResult, UsbOutResult};
use emulator::io::usb::hub::UsbHub;
use emulator::io::usb::hub::UsbHubDevice;
use emulator::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel};

const USB_REQUEST_GET_STATUS: u8 = 0x00;
const USB_REQUEST_CLEAR_FEATURE: u8 = 0x01;
const USB_REQUEST_SET_FEATURE: u8 = 0x03;
const USB_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;
const USB_REQUEST_GET_INTERFACE: u8 = 0x0a;
const USB_REQUEST_SET_INTERFACE: u8 = 0x0b;

const USB_DESCRIPTOR_TYPE_CONFIGURATION: u16 = 0x02;

const USB_FEATURE_ENDPOINT_HALT: u16 = 0;
const USB_FEATURE_DEVICE_REMOTE_WAKEUP: u16 = 1;

const HUB_PORT_FEATURE_ENABLE: u16 = 1;
const HUB_PORT_FEATURE_SUSPEND: u16 = 2;
const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;
const HUB_PORT_FEATURE_C_PORT_CONNECTION: u16 = 16;
const HUB_PORT_FEATURE_C_PORT_ENABLE: u16 = 17;
const HUB_PORT_FEATURE_C_PORT_SUSPEND: u16 = 18;
const HUB_PORT_FEATURE_C_PORT_OVER_CURRENT: u16 = 19;
const HUB_PORT_FEATURE_C_PORT_RESET: u16 = 20;

const HUB_PORT_STATUS_ENABLE: u16 = 1 << 1;
const HUB_PORT_STATUS_SUSPEND: u16 = 1 << 2;
const HUB_PORT_STATUS_POWER: u16 = 1 << 8;

const HUB_PORT_CHANGE_ENABLE: u16 = 1 << 1;
const HUB_PORT_CHANGE_SUSPEND: u16 = 1 << 2;

const HUB_INTERRUPT_IN_EP: u8 = 0x81;

#[derive(Default)]
struct DummyUsbDevice;

impl UsbDeviceModel for DummyUsbDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

fn setup(
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    w_length: u16,
) -> SetupPacket {
    SetupPacket {
        bm_request_type,
        b_request,
        w_value,
        w_index,
        w_length,
    }
}

fn set_configuration(config: u8) -> SetupPacket {
    setup(0x00, USB_REQUEST_SET_CONFIGURATION, config as u16, 0, 0)
}

fn standard_get_status_interface(interface: u16) -> SetupPacket {
    setup(0x81, USB_REQUEST_GET_STATUS, 0, interface, 2)
}

fn standard_get_interface(interface: u16) -> SetupPacket {
    setup(0x81, USB_REQUEST_GET_INTERFACE, 0, interface, 1)
}

fn standard_set_interface(interface: u16, alt_setting: u16) -> SetupPacket {
    setup(0x01, USB_REQUEST_SET_INTERFACE, alt_setting, interface, 0)
}

fn standard_get_status_endpoint(ep: u8) -> SetupPacket {
    setup(0x82, USB_REQUEST_GET_STATUS, 0, ep as u16, 2)
}

fn standard_set_feature_endpoint_halt(ep: u8) -> SetupPacket {
    setup(
        0x02,
        USB_REQUEST_SET_FEATURE,
        USB_FEATURE_ENDPOINT_HALT,
        ep as u16,
        0,
    )
}

fn standard_clear_feature_endpoint_halt(ep: u8) -> SetupPacket {
    setup(
        0x02,
        USB_REQUEST_CLEAR_FEATURE,
        USB_FEATURE_ENDPOINT_HALT,
        ep as u16,
        0,
    )
}

fn hub_get_status_port(port: u16) -> SetupPacket {
    setup(0xa3, USB_REQUEST_GET_STATUS, 0, port, 4)
}

fn hub_set_feature_port(port: u16, feature: u16) -> SetupPacket {
    setup(0x23, USB_REQUEST_SET_FEATURE, feature, port, 0)
}

fn hub_clear_feature_port(port: u16, feature: u16) -> SetupPacket {
    setup(0x23, USB_REQUEST_CLEAR_FEATURE, feature, port, 0)
}

fn hub_clear_feature_device(feature: u16) -> SetupPacket {
    setup(0x20, USB_REQUEST_CLEAR_FEATURE, feature, 0, 0)
}

fn get_port_status_and_change(hub: &mut UsbHubDevice, port: u16) -> (u16, u16) {
    let ControlResponse::Data(data) = hub.handle_control_request(hub_get_status_port(port), None)
    else {
        panic!("expected Data for hub port GET_STATUS");
    };
    assert_eq!(data.len(), 4);
    let status = u16::from_le_bytes([data[0], data[1]]);
    let change = u16::from_le_bytes([data[2], data[3]]);
    (status, change)
}

#[test]
fn usb_hub_standard_get_status_interface_returns_zeroes() {
    let mut hub = UsbHubDevice::new();
    let ControlResponse::Data(data) =
        hub.handle_control_request(standard_get_status_interface(0), None)
    else {
        panic!("expected Data response");
    };
    assert_eq!(data, [0, 0]);
}

#[test]
fn usb_hub_standard_device_remote_wakeup_feature_roundtrips() {
    let mut hub = UsbHubDevice::new();

    let ControlResponse::Data(st) =
        hub.handle_control_request(setup(0x80, USB_REQUEST_GET_STATUS, 0, 0, 2), None)
    else {
        panic!("expected Data response");
    };
    assert_eq!(st, [0, 0]);

    assert_eq!(
        hub.handle_control_request(
            setup(
                0x00,
                USB_REQUEST_SET_FEATURE,
                USB_FEATURE_DEVICE_REMOTE_WAKEUP,
                0,
                0,
            ),
            None,
        ),
        ControlResponse::Ack
    );

    let ControlResponse::Data(st) =
        hub.handle_control_request(setup(0x80, USB_REQUEST_GET_STATUS, 0, 0, 2), None)
    else {
        panic!("expected Data response");
    };
    assert_eq!(st, [0x02, 0x00]);

    assert_eq!(
        hub.handle_control_request(
            setup(
                0x00,
                USB_REQUEST_CLEAR_FEATURE,
                USB_FEATURE_DEVICE_REMOTE_WAKEUP,
                0,
                0,
            ),
            None,
        ),
        ControlResponse::Ack
    );

    let ControlResponse::Data(st) =
        hub.handle_control_request(setup(0x80, USB_REQUEST_GET_STATUS, 0, 0, 2), None)
    else {
        panic!("expected Data response");
    };
    assert_eq!(st, [0, 0]);
}

#[test]
fn usb_hub_standard_get_set_interface_roundtrips_alt_setting_zero() {
    let mut hub = UsbHubDevice::new();

    let ControlResponse::Data(data) = hub.handle_control_request(standard_get_interface(0), None)
    else {
        panic!("expected Data response");
    };
    assert_eq!(data, [0]);

    assert_eq!(
        hub.handle_control_request(standard_set_interface(0, 0), None),
        ControlResponse::Ack
    );

    // Non-existent interface / alt-setting should stall.
    assert_eq!(
        hub.handle_control_request(standard_get_interface(1), None),
        ControlResponse::Stall
    );
    assert_eq!(
        hub.handle_control_request(standard_set_interface(0, 1), None),
        ControlResponse::Stall
    );
}

#[test]
fn usb_hub_class_clear_feature_device_accepts_hub_change_selectors() {
    const HUB_FEATURE_C_HUB_LOCAL_POWER: u16 = 0;
    const HUB_FEATURE_C_HUB_OVER_CURRENT: u16 = 1;

    let mut hub = UsbHubDevice::new();

    for feature in [
        HUB_FEATURE_C_HUB_LOCAL_POWER,
        HUB_FEATURE_C_HUB_OVER_CURRENT,
    ] {
        assert_eq!(
            hub.handle_control_request(hub_clear_feature_device(feature), None),
            ControlResponse::Ack
        );
    }

    assert_eq!(
        hub.handle_control_request(hub_clear_feature_device(0x1234), None),
        ControlResponse::Stall
    );
}

#[test]
fn usb_hub_standard_endpoint_halt_controls_interrupt_polling() {
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(DummyUsbDevice));

    assert_eq!(
        hub.handle_control_request(set_configuration(1), None),
        ControlResponse::Ack
    );

    // Power the port so it becomes "connected" and raises a change bit.
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_POWER), None),
        ControlResponse::Ack
    );

    let UsbInResult::Data(bitmap) = hub.handle_in_transfer(HUB_INTERRUPT_IN_EP, 1) else {
        panic!("expected port-change bitmap");
    };
    assert_eq!(bitmap.len(), 1);
    assert_ne!(bitmap[0] & 0x02, 0); // bit1 = port1 change

    assert_eq!(
        hub.handle_control_request(
            standard_set_feature_endpoint_halt(HUB_INTERRUPT_IN_EP),
            None
        ),
        ControlResponse::Ack
    );

    assert_eq!(
        hub.handle_in_transfer(HUB_INTERRUPT_IN_EP, 1),
        UsbInResult::Stall
    );

    let ControlResponse::Data(st) =
        hub.handle_control_request(standard_get_status_endpoint(HUB_INTERRUPT_IN_EP), None)
    else {
        panic!("expected Data response");
    };
    assert_eq!(st, [1, 0]);

    assert_eq!(
        hub.handle_control_request(
            standard_clear_feature_endpoint_halt(HUB_INTERRUPT_IN_EP),
            None
        ),
        ControlResponse::Ack
    );

    assert_ne!(
        hub.handle_in_transfer(HUB_INTERRUPT_IN_EP, 1),
        UsbInResult::Stall,
        "clearing ENDPOINT_HALT should restore interrupt endpoint"
    );

    let ControlResponse::Data(st) =
        hub.handle_control_request(standard_get_status_endpoint(HUB_INTERRUPT_IN_EP), None)
    else {
        panic!("expected Data response");
    };
    assert_eq!(st, [0, 0]);

    let UsbInResult::Data(bitmap) = hub.handle_in_transfer(HUB_INTERRUPT_IN_EP, 1) else {
        panic!("expected port-change bitmap after clearing halt");
    };
    assert_ne!(bitmap[0] & 0x02, 0); // bit1 = port1 change
}

#[test]
fn usb_hub_standard_endpoint_requests_stall_for_unknown_endpoint() {
    let mut hub = UsbHubDevice::new();

    assert_eq!(
        hub.handle_control_request(standard_get_status_endpoint(0x82), None),
        ControlResponse::Stall
    );
    assert_eq!(
        hub.handle_control_request(standard_set_feature_endpoint_halt(0x82), None),
        ControlResponse::Stall
    );
    assert_eq!(
        hub.handle_control_request(standard_clear_feature_endpoint_halt(0x82), None),
        ControlResponse::Stall
    );
}

#[test]
fn usb_hub_clear_port_power_disables_routing_and_sets_enable_change() {
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(DummyUsbDevice));
    assert_eq!(
        hub.handle_control_request(set_configuration(1), None),
        ControlResponse::Ack
    );

    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_POWER), None),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_RESET), None),
        ControlResponse::Ack
    );
    for _ in 0..50 {
        UsbHub::tick_1ms(&mut hub);
    }

    // Clear any change bits from attach/reset so the power-off change is observable.
    for feature in [
        HUB_PORT_FEATURE_C_PORT_CONNECTION,
        HUB_PORT_FEATURE_C_PORT_ENABLE,
        HUB_PORT_FEATURE_C_PORT_RESET,
    ] {
        assert_eq!(
            hub.handle_control_request(hub_clear_feature_port(1, feature), None),
            ControlResponse::Ack
        );
    }

    assert!(hub.child_device_mut_for_address(0).is_some());

    assert_eq!(
        hub.handle_control_request(hub_clear_feature_port(1, HUB_PORT_FEATURE_POWER), None),
        ControlResponse::Ack
    );

    let (status, change) = get_port_status_and_change(&mut hub, 1);
    assert_eq!(status & HUB_PORT_STATUS_POWER, 0);
    assert_eq!(status & HUB_PORT_STATUS_ENABLE, 0);
    assert_ne!(change & HUB_PORT_CHANGE_ENABLE, 0);

    assert!(hub.child_device_mut_for_address(0).is_none());
}

#[test]
fn usb_hub_clear_port_power_resets_downstream_address() {
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(DummyUsbDevice));
    assert_eq!(
        hub.handle_control_request(set_configuration(1), None),
        ControlResponse::Ack
    );

    // Power + reset so the downstream device becomes routable.
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_POWER), None),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_RESET), None),
        ControlResponse::Ack
    );
    for _ in 0..50 {
        UsbHub::tick_1ms(&mut hub);
    }

    // Assign a non-zero address to the downstream device.
    {
        let dev = hub
            .child_device_mut_for_address(0)
            .expect("downstream device should be routable at address 0");
        let setup = SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 5,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
    }
    assert!(hub.child_device_mut_for_address(5).is_some());

    // Power off the port: device should be reset back to address 0 even though it remains attached.
    assert_eq!(
        hub.handle_control_request(hub_clear_feature_port(1, HUB_PORT_FEATURE_POWER), None),
        ControlResponse::Ack
    );
    assert!(hub.child_device_mut_for_address(5).is_none());
    assert_eq!(
        hub.downstream_device_mut(0)
            .expect("device should still be physically attached")
            .address(),
        0
    );
}

#[test]
fn usb_hub_port_enable_set_and_clear_feature() {
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(DummyUsbDevice));
    assert_eq!(
        hub.handle_control_request(set_configuration(1), None),
        ControlResponse::Ack
    );

    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_POWER), None),
        ControlResponse::Ack
    );

    // Clear initial connect-change so only enable-change is observed.
    assert_eq!(
        hub.handle_control_request(
            hub_clear_feature_port(1, HUB_PORT_FEATURE_C_PORT_CONNECTION),
            None
        ),
        ControlResponse::Ack
    );

    // Enable port (optional hub behavior, but Windows probes may issue this).
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_ENABLE), None),
        ControlResponse::Ack
    );
    let (status, change) = get_port_status_and_change(&mut hub, 1);
    assert_ne!(status & HUB_PORT_STATUS_ENABLE, 0);
    assert_ne!(change & HUB_PORT_CHANGE_ENABLE, 0);

    // Clear enable-change and then disable.
    assert_eq!(
        hub.handle_control_request(
            hub_clear_feature_port(1, HUB_PORT_FEATURE_C_PORT_ENABLE),
            None
        ),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(hub_clear_feature_port(1, HUB_PORT_FEATURE_ENABLE), None),
        ControlResponse::Ack
    );
    let (status, change) = get_port_status_and_change(&mut hub, 1);
    assert_eq!(status & HUB_PORT_STATUS_ENABLE, 0);
    assert_ne!(change & HUB_PORT_CHANGE_ENABLE, 0);

    // Clear enable-change and then re-enable.
    assert_eq!(
        hub.handle_control_request(
            hub_clear_feature_port(1, HUB_PORT_FEATURE_C_PORT_ENABLE),
            None
        ),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_ENABLE), None),
        ControlResponse::Ack
    );
    let (status, change) = get_port_status_and_change(&mut hub, 1);
    assert_ne!(status & HUB_PORT_STATUS_ENABLE, 0);
    assert_ne!(change & HUB_PORT_CHANGE_ENABLE, 0);
}

#[test]
fn usb_hub_port_suspend_set_and_clear_feature() {
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(DummyUsbDevice));
    assert_eq!(
        hub.handle_control_request(set_configuration(1), None),
        ControlResponse::Ack
    );

    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_POWER), None),
        ControlResponse::Ack
    );
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_ENABLE), None),
        ControlResponse::Ack
    );

    // Clear any change bits so suspend-change edges are observable.
    for feature in [
        HUB_PORT_FEATURE_C_PORT_CONNECTION,
        HUB_PORT_FEATURE_C_PORT_ENABLE,
    ] {
        assert_eq!(
            hub.handle_control_request(hub_clear_feature_port(1, feature), None),
            ControlResponse::Ack
        );
    }

    // Suspend the port.
    assert_eq!(
        hub.handle_control_request(hub_set_feature_port(1, HUB_PORT_FEATURE_SUSPEND), None),
        ControlResponse::Ack
    );
    let (status, change) = get_port_status_and_change(&mut hub, 1);
    assert_ne!(status & HUB_PORT_STATUS_SUSPEND, 0);
    assert_ne!(change & HUB_PORT_CHANGE_SUSPEND, 0);

    // Clear suspend-change.
    assert_eq!(
        hub.handle_control_request(
            hub_clear_feature_port(1, HUB_PORT_FEATURE_C_PORT_SUSPEND),
            None
        ),
        ControlResponse::Ack
    );
    let (status, change) = get_port_status_and_change(&mut hub, 1);
    assert_ne!(status & HUB_PORT_STATUS_SUSPEND, 0);
    assert_eq!(change & HUB_PORT_CHANGE_SUSPEND, 0);

    // Resume (clear suspend).
    assert_eq!(
        hub.handle_control_request(hub_clear_feature_port(1, HUB_PORT_FEATURE_SUSPEND), None),
        ControlResponse::Ack
    );
    let (status, change) = get_port_status_and_change(&mut hub, 1);
    assert_eq!(status & HUB_PORT_STATUS_SUSPEND, 0);
    assert_ne!(change & HUB_PORT_CHANGE_SUSPEND, 0);

    // Clearing unimplemented change selectors should not stall.
    assert_eq!(
        hub.handle_control_request(
            hub_clear_feature_port(1, HUB_PORT_FEATURE_C_PORT_OVER_CURRENT),
            None
        ),
        ControlResponse::Ack
    );
}

#[test]
fn usb_hub_hub_descriptor_fields_are_stable_and_correct_length() {
    const HUB_DESCRIPTOR_TYPE: u16 = 0x29;
    const HUB_NUM_PORTS: usize = 4;
    const HUB_W_HUB_CHARACTERISTICS: u16 = 0x0011;
    const HUB_PORT_PWR_CTRL_MASK: u8 = ((1u32 << (HUB_NUM_PORTS + 1)) - 2) as u8;

    let mut hub = UsbHubDevice::new();

    let ControlResponse::Data(desc) = hub.handle_control_request(
        setup(
            0xa0,
            USB_REQUEST_GET_DESCRIPTOR,
            HUB_DESCRIPTOR_TYPE << 8,
            0,
            64,
        ),
        None,
    ) else {
        panic!("expected Data response");
    };

    assert_eq!(desc.len(), 9);
    assert_eq!(desc[0], 9);
    assert_eq!(desc[1], HUB_DESCRIPTOR_TYPE as u8);
    assert_eq!(desc[2], HUB_NUM_PORTS as u8);
    assert_eq!(
        u16::from_le_bytes([desc[3], desc[4]]),
        HUB_W_HUB_CHARACTERISTICS
    );

    // DeviceRemovable + PortPwrCtrlMask bitmaps for 4 ports are 1 byte each.
    assert_eq!(desc[7], 0x00);
    assert_eq!(desc[8], HUB_PORT_PWR_CTRL_MASK);

    // Configuration descriptor should expose a non-zero bMaxPower.
    let ControlResponse::Data(cfg) = hub.handle_control_request(
        setup(
            0x80,
            USB_REQUEST_GET_DESCRIPTOR,
            USB_DESCRIPTOR_TYPE_CONFIGURATION << 8,
            0,
            255,
        ),
        None,
    ) else {
        panic!("expected configuration descriptor response");
    };
    assert_eq!(cfg[8], 50);
}

#[test]
fn usb_hub_standard_get_descriptor_accepts_hub_descriptor_type() {
    const HUB_DESCRIPTOR_TYPE: u16 = 0x29;

    let mut hub = UsbHubDevice::new();

    let ControlResponse::Data(class_desc) = hub.handle_control_request(
        setup(
            0xa0,
            USB_REQUEST_GET_DESCRIPTOR,
            HUB_DESCRIPTOR_TYPE << 8,
            0,
            64,
        ),
        None,
    ) else {
        panic!("expected Data response");
    };

    let ControlResponse::Data(std_desc) = hub.handle_control_request(
        setup(
            0x80,
            USB_REQUEST_GET_DESCRIPTOR,
            HUB_DESCRIPTOR_TYPE << 8,
            0,
            64,
        ),
        None,
    ) else {
        panic!("expected Data response");
    };

    assert_eq!(std_desc, class_desc);
    assert_eq!(std_desc[1], HUB_DESCRIPTOR_TYPE as u8);
}

#[test]
fn usb_hub_interrupt_bitmap_scales_with_port_count() {
    const HUB_DESCRIPTOR_TYPE: u16 = 0x29;

    let mut hub = UsbHubDevice::with_port_count(16);
    hub.attach(16, Box::new(DummyUsbDevice));

    assert_eq!(
        hub.handle_control_request(set_configuration(1), None),
        ControlResponse::Ack
    );

    let UsbInResult::Data(bitmap) = hub.handle_in_transfer(HUB_INTERRUPT_IN_EP, 3) else {
        panic!("expected port-change bitmap");
    };
    assert_eq!(bitmap.len(), 3);
    assert_ne!(bitmap[2] & 0x01, 0); // bit16 = port16 change.

    let ControlResponse::Data(desc) = hub.handle_control_request(
        setup(
            0xa0,
            USB_REQUEST_GET_DESCRIPTOR,
            HUB_DESCRIPTOR_TYPE << 8,
            0,
            64,
        ),
        None,
    ) else {
        panic!("expected Data response");
    };

    assert_eq!(desc.len(), 13);
    assert_eq!(desc[0], 13);
    assert_eq!(desc[1], HUB_DESCRIPTOR_TYPE as u8);
    assert_eq!(desc[2], 16);

    // DeviceRemovable + PortPwrCtrlMask bitmaps for 16 ports are 3 bytes each.
    assert_eq!(desc[7], 0x00);
    assert_eq!(desc[8], 0x00);
    assert_eq!(desc[9], 0x00);
    assert_eq!(desc[10], 0xFE);
    assert_eq!(desc[11], 0xFF);
    assert_eq!(desc[12], 0x01);

    // Interrupt endpoint wMaxPacketSize should match the bitmap length.
    let ControlResponse::Data(cfg) = hub.handle_control_request(
        setup(
            0x80,
            USB_REQUEST_GET_DESCRIPTOR,
            USB_DESCRIPTOR_TYPE_CONFIGURATION << 8,
            0,
            255,
        ),
        None,
    ) else {
        panic!("expected configuration descriptor response");
    };
    assert_eq!(cfg[22], 3);
    assert_eq!(cfg[23], 0);
}

#[test]
fn usb_hub_interrupt_bitmap_respects_max_len() {
    let mut hub = UsbHubDevice::with_port_count(8);
    hub.attach(1, Box::new(DummyUsbDevice));

    assert_eq!(
        hub.handle_control_request(set_configuration(1), None),
        ControlResponse::Ack
    );

    let UsbInResult::Data(bitmap) = hub.handle_in_transfer(HUB_INTERRUPT_IN_EP, 1) else {
        panic!("expected port-change bitmap");
    };
    assert_eq!(bitmap.len(), 1);
    assert_ne!(bitmap[0] & 0x02, 0); // bit1 = port1 change
}
