#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[test]
fn inject_input_batch_tracks_usb_keyboard_state_before_configuration_when_ps2_is_absent() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Disable PS/2 so `inject_input_batch` has to fall back to the synthetic USB keyboard.
        enable_i8042: false,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_virtio_input: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        ..Default::default()
    })
    .unwrap();

    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic USB keyboard should be present");
    assert!(!kbd.configured());
    assert_eq!(kbd.handle_interrupt_in(0x81), UsbInResult::Nak);

    // Press 'A' (usage 0x04) while the USB keyboard is unconfigured. The device model should track
    // the pressed state but not emit interrupt-IN reports yet.
    let words_press: [u32; 6] = [1, 0, 6, 0, 0x0104, 0];
    m.inject_input_batch(&words_press);
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Nak,
        "unconfigured USB device must not emit interrupt reports"
    );

    // Configure the keyboard and ensure the held state becomes visible without requiring another
    // key event.
    let set_cfg = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09, // SET_CONFIGURATION
        w_value: 0x0001,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        kbd.handle_control_request(set_cfg, None),
        ControlResponse::Ack
    );
    assert!(kbd.configured());
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0, 0, 0x04, 0, 0, 0, 0, 0]),
        "expected held key to be visible immediately after SET_CONFIGURATION"
    );

    // Release 'A' and ensure the cleared state is observable via a new report.
    let words_release: [u32; 6] = [1, 0, 6, 0, 0x0004, 0];
    m.inject_input_batch(&words_release);
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0, 0, 0, 0, 0, 0, 0, 0])
    );
}
