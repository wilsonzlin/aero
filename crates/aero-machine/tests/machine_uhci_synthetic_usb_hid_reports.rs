#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

fn synthetic_usb_hid_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for this device-model test.
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_virtio_net: false,
        enable_e1000: false,
        enable_vga: false,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

fn configure_device(dev: &mut dyn aero_usb::UsbDeviceModel) {
    assert_eq!(
        dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00, // HostToDevice | Standard | Device
                b_request: 0x09,       // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack,
        "device should accept SET_CONFIGURATION"
    );
}

#[test]
fn machine_synthetic_usb_hid_mouse_button_injection_produces_report() {
    let cfg = synthetic_usb_hid_cfg();
    let mut m = Machine::new(cfg).unwrap();

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic USB HID mouse handle should be present");
    configure_device(&mut mouse);

    // Press left (bit0).
    m.inject_usb_hid_mouse_buttons(0x01);
    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x01, 0x00, 0x00, 0x00, 0x00]);

    // Release.
    m.inject_usb_hid_mouse_buttons(0x00);
    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn machine_synthetic_usb_hid_gamepad_injection_produces_report() {
    let cfg = synthetic_usb_hid_cfg();
    let mut m = Machine::new(cfg).unwrap();

    let mut gamepad = m
        .usb_hid_gamepad_handle()
        .expect("synthetic USB HID gamepad handle should be present");
    configure_device(&mut gamepad);

    // Pack an 8-byte report as two u32s to match the public `Machine` injection API.
    let report = [0x03, 0x00, 0x02, 0x01, 0x02, 0x03, 0x04, 0x00];
    let a = u32::from_le_bytes(report[0..4].try_into().expect("len checked"));
    let b = u32::from_le_bytes(report[4..8].try_into().expect("len checked"));
    m.inject_usb_hid_gamepad_report(a, b);

    let got = match gamepad.handle_in_transfer(0x81, 8) {
        UsbInResult::Data(data) => data,
        other => panic!("expected gamepad report data, got {other:?}"),
    };
    assert_eq!(got, report);
}
