#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[test]
fn inject_input_batch_tracks_usb_consumer_control_state_before_configuration() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        enable_virtio_input: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        ..Default::default()
    })
    .unwrap();

    let mut consumer = m
        .usb_hid_consumer_control_handle()
        .expect("synthetic USB consumer-control device should be present");
    assert!(!consumer.configured());
    assert_eq!(consumer.handle_interrupt_in(0x81), UsbInResult::Nak);

    // Press Volume Up (Consumer Page 0x0C, Usage ID 0x00E9) while unconfigured. The device model
    // should track the pressed state but not emit an interrupt-IN report yet.
    let words_press: [u32; 6] = [1, 0, 7, 0, 0x0001_000c, 0x00e9];
    m.inject_input_batch(&words_press);
    assert_eq!(
        consumer.handle_interrupt_in(0x81),
        UsbInResult::Nak,
        "unconfigured USB device must not emit interrupt reports"
    );

    // Configure the consumer-control device and ensure the held state becomes visible immediately.
    let set_cfg = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09, // SET_CONFIGURATION
        w_value: 0x0001,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        consumer.handle_control_request(set_cfg, None),
        ControlResponse::Ack
    );
    assert!(consumer.configured());
    assert_eq!(
        consumer.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0xE9, 0x00]),
        "expected held consumer usage to be visible immediately after SET_CONFIGURATION"
    );

    // Release and ensure the cleared state is reported.
    let words_release: [u32; 6] = [1, 0, 7, 0, 0x0000_000c, 0x00e9];
    m.inject_input_batch(&words_release);
    assert_eq!(
        consumer.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0x00, 0x00])
    );
}
