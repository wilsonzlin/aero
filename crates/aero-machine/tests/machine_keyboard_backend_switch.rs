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
fn inject_input_batch_keyboard_release_stays_on_usb_when_virtio_becomes_ready() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Disable i8042 so `inject_input_batch` prefers USB keyboard injection.
        enable_i8042: false,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        ..Default::default()
    })
    .unwrap();

    assert!(
        !m.virtio_input_keyboard_driver_ok(),
        "virtio keyboard should start without DRIVER_OK"
    );

    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic USB keyboard should be present");

    // Configure the keyboard so it can emit interrupt-IN reports.
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

    // Enable periodic reports so we can observe the keyboard's *current* pressed state even if no
    // new reports are queued (useful for detecting stuck keys).
    let set_idle = SetupPacket {
        bm_request_type: 0x21, // HostToDevice | Class | Interface
        b_request: 0x0a,       // SET_IDLE
        w_value: 0x0100,       // idle_rate=1 (4ms), report_id=0
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        kbd.handle_control_request(set_idle, None),
        ControlResponse::Ack
    );

    // Batch with KeyHidUsage press for 'A' (usage 0x04).
    let words_press: [u32; 6] = [
        1, 0, // header: count=1, timestamp unused
        6, 0, // type=KeyHidUsage, event_ts unused
        0x0104, 0, // a=(usage=0x04)|(pressed<<8)
    ];
    m.inject_input_batch(&words_press);
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0, 0, 0x04, 0, 0, 0, 0, 0])
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

    // Batch with KeyHidUsage release for 'A'.
    let words_release: [u32; 6] = [1, 0, 6, 0, 0x0004, 0];
    m.inject_input_batch(&words_release);

    // Advance time so the idle report fires (>= 4ms).
    for _ in 0..8 {
        kbd.tick_1ms();
    }
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0, 0, 0, 0, 0, 0, 0, 0]),
        "expected key release to clear USB state even if virtio becomes ready mid-hold"
    );
}

#[test]
fn inject_input_batch_keyboard_release_after_snapshot_restore_clears_usb_even_if_virtio_becomes_ready(
) {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Disable i8042 so `inject_input_batch` prefers USB keyboard injection.
        enable_i8042: false,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();
    assert!(
        !src.virtio_input_keyboard_driver_ok(),
        "virtio keyboard should start without DRIVER_OK"
    );

    let mut kbd = src
        .usb_hid_keyboard_handle()
        .expect("synthetic USB keyboard should be present");

    // Configure the keyboard so it can emit interrupt-IN reports.
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

    // Enable periodic reports so we can observe the keyboard's pressed state after restore even if
    // the release event is routed to a different backend.
    let set_idle = SetupPacket {
        bm_request_type: 0x21, // HostToDevice | Class | Interface
        b_request: 0x0a,       // SET_IDLE
        w_value: 0x0100,       // idle_rate=1 (4ms), report_id=0
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        kbd.handle_control_request(set_idle, None),
        ControlResponse::Ack
    );

    // Press 'A' (usage 0x04) while virtio-input is not ready; this should route to the USB HID
    // keyboard and leave it in a pressed state.
    let words_press: [u32; 6] = [1, 0, 6, 0, 0x0104, 0];
    src.inject_input_batch(&words_press);
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0, 0, 0x04, 0, 0, 0, 0, 0])
    );

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let mut kbd = restored
        .usb_hid_keyboard_handle()
        .expect("synthetic USB keyboard should be present after restore");
    if !kbd.configured() {
        assert_eq!(
            kbd.handle_control_request(set_cfg, None),
            ControlResponse::Ack
        );
    }
    assert_eq!(
        kbd.handle_control_request(set_idle, None),
        ControlResponse::Ack
    );
    assert_eq!(kbd.handle_interrupt_in(0x81), UsbInResult::Nak);

    // Flip virtio keyboard to DRIVER_OK before releasing (simulating a backend switch). Snapshot
    // restore drops host-side pressed-key tracking, so the release would otherwise be routed to the
    // new backend and leave the USB model stuck.
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

    // Release 'A' after restore.
    let words_release: [u32; 6] = [1, 0, 6, 0, 0x0004, 0];
    restored.inject_input_batch(&words_release);

    // Advance time so the idle report fires (>= 4ms).
    for _ in 0..8 {
        kbd.tick_1ms();
    }
    assert_eq!(
        kbd.handle_interrupt_in(0x81),
        UsbInResult::Data(vec![0, 0, 0, 0, 0, 0, 0, 0]),
        "expected key release after snapshot restore to clear USB state even if virtio becomes ready"
    );
}
