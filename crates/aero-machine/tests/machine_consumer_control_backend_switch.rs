#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};
use aero_virtio::pci::VIRTIO_STATUS_DRIVER_OK;

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_write(0xCFC + (offset & 3), size, value);
}

fn bar0_base(m: &mut Machine, bdf: PciBdf) -> u64 {
    let bar0_lo = cfg_read(m, bdf, 0x10, 4);
    let bar0_hi = cfg_read(m, bdf, 0x14, 4);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xFFFF_FFF0)
}

#[test]
fn inject_browser_key_consumer_release_stays_on_usb_when_virtio_becomes_ready() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg.clone()).unwrap();

    assert!(
        !m.virtio_input_keyboard_driver_ok(),
        "virtio keyboard should start without DRIVER_OK"
    );

    let mut consumer = m
        .usb_hid_consumer_control_handle()
        .expect("synthetic USB consumer-control device should be present");

    // Configure the consumer-control device directly (bypass full USB enumeration; we only need
    // interrupt-IN report behavior).
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
    assert_eq!(consumer.handle_interrupt_in(0x81), UsbInResult::Nak);

    // Press a media key before virtio-input is ready; this should route via the USB consumer-control
    // device.
    m.inject_browser_key("AudioVolumeUp", true);
    assert_eq!(
        consumer.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0xE9, 0x00])
    );

    // Flip virtio keyboard to DRIVER_OK between press and release.
    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut m, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");
    let mut cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cmd |= 0x0006; // MEM + BUSMASTER
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd));
    // device_status lives in common config at BAR0+0x14.
    m.write_physical_u8(bar0 + 0x14, VIRTIO_STATUS_DRIVER_OK);
    assert!(m.virtio_input_keyboard_driver_ok(), "expected DRIVER_OK");

    // Release should still route to the USB consumer-control device (matching the press), ensuring
    // the USB model's pressed state is cleared rather than being left "stuck".
    m.inject_browser_key("AudioVolumeUp", false);
    assert_eq!(
        consumer.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0x00, 0x00])
    );
}

#[test]
fn inject_browser_key_consumer_release_after_snapshot_restore_clears_usb_even_if_virtio_becomes_ready(
) {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    let mut consumer = src
        .usb_hid_consumer_control_handle()
        .expect("synthetic USB consumer-control device should be present");
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

    // Press before snapshot while virtio-input is not ready; this should route to the USB
    // consumer-control device and leave the device in a pressed state.
    src.inject_browser_key("AudioVolumeUp", true);
    assert_eq!(
        consumer.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0xE9, 0x00])
    );

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let mut consumer = restored
        .usb_hid_consumer_control_handle()
        .expect("synthetic USB consumer-control device should be present after restore");
    if !consumer.configured() {
        assert_eq!(
            consumer.handle_control_request(set_cfg, None),
            ControlResponse::Ack
        );
    }
    assert_eq!(consumer.handle_interrupt_in(0x81), UsbInResult::Nak);

    // Flip virtio keyboard to DRIVER_OK before releasing (simulating a backend switch).
    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let bar0 = bar0_base(&mut restored, bdf);
    assert_ne!(bar0, 0, "virtio-input BAR0 must be assigned by BIOS POST");
    let mut cmd = cfg_read(&mut restored, bdf, 0x04, 2) as u16;
    cmd |= 0x0006; // MEM + BUSMASTER
    cfg_write(&mut restored, bdf, 0x04, 2, u32::from(cmd));
    restored.write_physical_u8(bar0 + 0x14, VIRTIO_STATUS_DRIVER_OK);
    assert!(
        restored.virtio_input_keyboard_driver_ok(),
        "expected DRIVER_OK"
    );

    // Release after restore should still clear the USB consumer-control device (even though the
    // press->release pairing state is not preserved across snapshot restore).
    restored.inject_browser_key("AudioVolumeUp", false);
    assert_eq!(
        consumer.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0x00, 0x00])
    );
}
