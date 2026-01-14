use aero_usb::device::{AttachedUsbDevice, UsbOutResult};
use aero_usb::hid::passthrough::UsbHidPassthroughHandle;
use aero_usb::{SetupPacket, UsbInResult};

const USB_REQUEST_GET_STATUS: u8 = 0x00;
const USB_REQUEST_CLEAR_FEATURE: u8 = 0x01;
const USB_REQUEST_SET_FEATURE: u8 = 0x03;
const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;

const USB_FEATURE_ENDPOINT_HALT: u16 = 0x0000;
const USB_FEATURE_DEVICE_REMOTE_WAKEUP: u16 = 0x0001;

const INTERRUPT_IN_EP_ADDR: u16 = 0x0081;
const INTERRUPT_IN_EP_NUM: u8 = 1;

fn sample_report_descriptor() -> Vec<u8> {
    // Minimal HID report descriptor with a single 1-byte input report.
    vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x00, // Usage (Undefined)
        0xa1, 0x01, // Collection (Application)
        0x09, 0x00, // Usage (Undefined)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x01, // Report Count (1)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

fn control_out_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(matches!(
        dev.handle_in(0, 0),
        UsbInResult::Data(data) if data.is_empty()
    ));
}

fn control_in(dev: &mut AttachedUsbDevice, setup: SetupPacket) -> Vec<u8> {
    const MAX_PACKET: usize = 64;

    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let requested = setup.w_length as usize;
    let mut out = Vec::new();

    if requested != 0 {
        loop {
            match dev.handle_in(0, MAX_PACKET) {
                UsbInResult::Data(chunk) => {
                    out.extend_from_slice(&chunk);
                    if out.len() >= requested || chunk.len() < MAX_PACKET {
                        break;
                    }
                }
                UsbInResult::Nak => continue,
                UsbInResult::Stall => panic!("unexpected STALL during control IN transfer"),
                UsbInResult::Timeout => panic!("unexpected TIMEOUT during control IN transfer"),
            }
        }
    }

    // Status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    out
}

fn u16_le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("expected 2 bytes"))
}

#[test]
fn passthrough_remote_wakeup_toggles_get_status_bit1() {
    let handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_report_descriptor(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(handle.clone()));

    // Typical enumeration flow configures the device before interacting with its endpoints.
    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00, // HostToDevice | Standard | Device
            b_request: USB_REQUEST_SET_CONFIGURATION,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(handle.configured());

    let status0 = u16_le(&control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: USB_REQUEST_GET_STATUS,
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
    ));
    assert_eq!(status0 & (1 << 1), 0);

    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00, // HostToDevice | Standard | Device
            b_request: USB_REQUEST_SET_FEATURE,
            w_value: USB_FEATURE_DEVICE_REMOTE_WAKEUP,
            w_index: 0,
            w_length: 0,
        },
    );

    let status1 = u16_le(&control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: USB_REQUEST_GET_STATUS,
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
    ));
    assert_ne!(status1 & (1 << 1), 0);

    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00, // HostToDevice | Standard | Device
            b_request: USB_REQUEST_CLEAR_FEATURE,
            w_value: USB_FEATURE_DEVICE_REMOTE_WAKEUP,
            w_index: 0,
            w_length: 0,
        },
    );

    let status2 = u16_le(&control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: USB_REQUEST_GET_STATUS,
            w_value: 0,
            w_index: 0,
            w_length: 2,
        },
    ));
    assert_eq!(status2 & (1 << 1), 0);
}

#[test]
fn passthrough_endpoint_halt_stalls_interrupt_in_until_cleared() {
    let handle = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_report_descriptor(),
        false,
        None,
        None,
        None,
    );
    let mut dev = AttachedUsbDevice::new(Box::new(handle.clone()));

    // Configure the device so the interrupt endpoints are active.
    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00, // HostToDevice | Standard | Device
            b_request: USB_REQUEST_SET_CONFIGURATION,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(handle.configured());

    assert_eq!(dev.handle_in(INTERRUPT_IN_EP_NUM, 64), UsbInResult::Nak);

    // SET_FEATURE(ENDPOINT_HALT) for the interrupt IN endpoint.
    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x02, // HostToDevice | Standard | Endpoint
            b_request: USB_REQUEST_SET_FEATURE,
            w_value: USB_FEATURE_ENDPOINT_HALT,
            w_index: INTERRUPT_IN_EP_ADDR,
            w_length: 0,
        },
    );

    let halted_status = u16_le(&control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x82, // DeviceToHost | Standard | Endpoint
            b_request: USB_REQUEST_GET_STATUS,
            w_value: 0,
            w_index: INTERRUPT_IN_EP_ADDR,
            w_length: 2,
        },
    ));
    assert_eq!(halted_status & 1, 1);

    assert_eq!(dev.handle_in(INTERRUPT_IN_EP_NUM, 64), UsbInResult::Stall);

    // CLEAR_FEATURE(ENDPOINT_HALT) should resume NAKing when no reports are queued.
    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x02, // HostToDevice | Standard | Endpoint
            b_request: USB_REQUEST_CLEAR_FEATURE,
            w_value: USB_FEATURE_ENDPOINT_HALT,
            w_index: INTERRUPT_IN_EP_ADDR,
            w_length: 0,
        },
    );

    let unhalted_status = u16_le(&control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x82, // DeviceToHost | Standard | Endpoint
            b_request: USB_REQUEST_GET_STATUS,
            w_value: 0,
            w_index: INTERRUPT_IN_EP_ADDR,
            w_length: 2,
        },
    ));
    assert_eq!(unhalted_status & 1, 0);

    assert_eq!(dev.handle_in(INTERRUPT_IN_EP_NUM, 64), UsbInResult::Nak);
}
