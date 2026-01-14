#![cfg(not(target_arch = "wasm32"))]

use std::any::Any;

use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::usb::uhci::regs;
use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbHubAttachError, UsbInResult};

fn uhci_io_base(m: &Machine) -> u16 {
    let pci_cfg = m
        .pci_config_ports()
        .expect("pc platform should expose pci_cfg");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let cfg = pci_cfg
        .bus_mut()
        .device_config(USB_UHCI_PIIX3.bdf)
        .expect("UHCI PCI function should exist");
    let bar4_base = cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
    assert_ne!(bar4_base, 0, "UHCI BAR4 base should be assigned by BIOS");
    u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
}

fn enable_uhci_io_decode(m: &mut Machine) {
    let pci_cfg = m
        .pci_config_ports()
        .expect("pc platform should expose pci_cfg");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();
    let cmd = bus.read_config(USB_UHCI_PIIX3.bdf, 0x04, 2) as u16;
    // PCI COMMAND bit0: I/O space enable.
    bus.write_config(USB_UHCI_PIIX3.bdf, 0x04, 2, u32::from(cmd | 0x0001));
}

fn configure_keyboard_for_reports(kbd: &mut aero_usb::hid::UsbHidKeyboardHandle) {
    if kbd.configured() {
        return;
    }
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = kbd.handle_control_request(setup, None);
    assert!(
        matches!(resp, ControlResponse::Ack),
        "expected SET_CONFIGURATION to ACK; got {resp:?}"
    );
    assert!(kbd.configured(), "keyboard should now be configured");
}

fn configure_mouse_for_reports(mouse: &mut aero_usb::hid::UsbHidMouseHandle) {
    if mouse.configured() {
        return;
    }
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = mouse.handle_control_request(setup, None);
    assert!(
        matches!(resp, ControlResponse::Ack),
        "expected SET_CONFIGURATION to ACK; got {resp:?}"
    );
    assert!(mouse.configured(), "mouse should now be configured");
}

fn configure_gamepad_for_reports(gamepad: &mut aero_usb::hid::UsbHidGamepadHandle) {
    if gamepad.configured() {
        return;
    }
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = gamepad.handle_control_request(setup, None);
    assert!(
        matches!(resp, ControlResponse::Ack),
        "expected SET_CONFIGURATION to ACK; got {resp:?}"
    );
    assert!(gamepad.configured(), "gamepad should now be configured");
}

fn configure_consumer_for_reports(consumer: &mut aero_usb::hid::UsbHidConsumerControlHandle) {
    if consumer.configured() {
        return;
    }
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let resp = consumer.handle_control_request(setup, None);
    assert!(
        matches!(resp, ControlResponse::Ack),
        "expected SET_CONFIGURATION to ACK; got {resp:?}"
    );
    assert!(
        consumer.configured(),
        "consumer-control device should now be configured"
    );
}

fn poll_keyboard_interrupt_in(m: &mut Machine) -> UsbInResult {
    let uhci = m.uhci().expect("UHCI device should exist");
    let mut uhci = uhci.borrow_mut();
    let root = uhci.controller_mut().hub_mut();
    let mut dev0 = root
        .port_device_mut(Machine::UHCI_EXTERNAL_HUB_ROOT_PORT as usize)
        .expect("UHCI root port 0 should have an external hub attached");
    let hub = dev0
        .as_hub_mut()
        .expect("root port 0 device should be a hub");
    let keyboard = hub
        .downstream_device_mut((Machine::UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT - 1) as usize)
        .expect("hub port 1 should contain a keyboard device");
    keyboard.model_mut().handle_interrupt_in(0x81)
}

fn poll_mouse_interrupt_in(m: &mut Machine) -> UsbInResult {
    let uhci = m.uhci().expect("UHCI device should exist");
    let mut uhci = uhci.borrow_mut();
    let root = uhci.controller_mut().hub_mut();
    let mut dev0 = root
        .port_device_mut(0)
        .expect("UHCI root port 0 should have an external hub attached");
    let hub = dev0
        .as_hub_mut()
        .expect("root port 0 device should be a hub");
    let mouse = hub
        .downstream_device_mut(1)
        .expect("hub port 2 should contain a mouse device");
    mouse.model_mut().handle_interrupt_in(0x81)
}

fn poll_gamepad_interrupt_in(m: &mut Machine) -> UsbInResult {
    let uhci = m.uhci().expect("UHCI device should exist");
    let mut uhci = uhci.borrow_mut();
    let root = uhci.controller_mut().hub_mut();
    let mut dev0 = root
        .port_device_mut(0)
        .expect("UHCI root port 0 should have an external hub attached");
    let hub = dev0
        .as_hub_mut()
        .expect("root port 0 device should be a hub");
    let gamepad = hub
        .downstream_device_mut(2)
        .expect("hub port 3 should contain a gamepad device");
    gamepad.model_mut().handle_interrupt_in(0x81)
}

fn poll_consumer_interrupt_in(m: &mut Machine) -> UsbInResult {
    let uhci = m.uhci().expect("UHCI device should exist");
    let mut uhci = uhci.borrow_mut();
    let root = uhci.controller_mut().hub_mut();
    let mut dev0 = root
        .port_device_mut(Machine::UHCI_EXTERNAL_HUB_ROOT_PORT as usize)
        .expect("UHCI root port 0 should have an external hub attached");
    let hub = dev0
        .as_hub_mut()
        .expect("root port 0 device should be a hub");
    let consumer = hub
        .downstream_device_mut((Machine::UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT - 1) as usize)
        .expect("hub port 4 should contain a consumer-control device");
    consumer.model_mut().handle_interrupt_in(0x81)
}

fn expect_keyboard_report_contains(result: UsbInResult, usage: u8, context: &str) {
    match result {
        UsbInResult::Data(data) => {
            assert_eq!(
                data.len(),
                8,
                "{context}: expected 8-byte keyboard report, got {} bytes",
                data.len()
            );
            assert!(
                data[2..].contains(&usage),
                "{context}: expected keyboard report to contain usage {usage:#04x}; got {data:?}"
            );
        }
        other => panic!("{context}: expected keyboard report data, got {other:?}"),
    }
}

fn expect_mouse_report(result: UsbInResult, expected: &[u8], context: &str) {
    match result {
        UsbInResult::Data(data) => assert_eq!(
            data, expected,
            "{context}: expected mouse report {expected:?}, got {data:?}"
        ),
        other => panic!("{context}: expected mouse report data, got {other:?}"),
    }
}

fn expect_gamepad_report(result: UsbInResult, expected: &[u8; 8], context: &str) {
    match result {
        UsbInResult::Data(data) => assert_eq!(
            data, expected,
            "{context}: expected gamepad report {expected:?}, got {data:?}"
        ),
        other => panic!("{context}: expected gamepad report data, got {other:?}"),
    }
}

fn inject_gamepad_report_bytes(m: &mut Machine, report: &[u8; 8]) {
    let a = u32::from_le_bytes(report[0..4].try_into().expect("len checked"));
    let b = u32::from_le_bytes(report[4..8].try_into().expect("len checked"));
    m.inject_usb_hid_gamepad_report(a, b);
}

fn expect_consumer_control_report(result: UsbInResult, usage: u16, context: &str) {
    let expected = usage.to_le_bytes().to_vec();
    match result {
        UsbInResult::Data(data) => assert_eq!(
            data, expected,
            "{context}: expected consumer-control report {expected:?}, got {data:?}"
        ),
        other => panic!("{context}: expected consumer-control report data, got {other:?}"),
    }
}

fn synthetic_usb_hid_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for topology tests.
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

#[test]
fn uhci_synthetic_usb_hid_topology_is_attached_on_boot() {
    let mut m = Machine::new(synthetic_usb_hid_cfg()).unwrap();
    enable_uhci_io_decode(&mut m);

    let base = uhci_io_base(&m);
    let portsc1 = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc1, 0xFFFF,
        "PORTSC1 should not read as open bus (ensure PCI I/O decode is enabled)"
    );
    assert_ne!(portsc1 & 0x0001, 0, "PORTSC1.CCS should be set");
    assert_ne!(portsc1 & 0x0002, 0, "PORTSC1.CSC should be set");

    // Root port 0 should contain an external hub with the runtime port count.
    {
        let uhci = m.uhci().expect("UHCI device should exist");
        let mut uhci = uhci.borrow_mut();
        let root = uhci.controller_mut().hub_mut();
        let mut dev0 = root
            .port_device_mut(Machine::UHCI_EXTERNAL_HUB_ROOT_PORT as usize)
            .expect("UHCI root port 0 should have an external hub attached");
        assert!(
            (dev0.model() as &dyn Any).is::<aero_usb::hub::UsbHubDevice>(),
            "root port 0 should host aero_usb::hub::UsbHubDevice"
        );

        let hub = dev0
            .as_hub_mut()
            .expect("root port 0 device should be a hub");
        assert_eq!(
            hub.num_ports(),
            usize::from(Machine::UHCI_EXTERNAL_HUB_PORT_COUNT),
            "external hub port count should match web runtime"
        );

        let kbd = hub
            .downstream_device_mut((Machine::UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT - 1) as usize)
            .expect("hub port 1 should contain a keyboard device");
        assert!(
            (kbd.model() as &dyn Any).is::<aero_usb::hid::UsbHidKeyboardHandle>(),
            "hub port 1 should host UsbHidKeyboardHandle"
        );

        let mouse = hub
            .downstream_device_mut((Machine::UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT - 1) as usize)
            .expect("hub port 2 should contain a mouse device");
        assert!(
            (mouse.model() as &dyn Any).is::<aero_usb::hid::UsbHidMouseHandle>(),
            "hub port 2 should host UsbHidMouseHandle"
        );

        let gamepad = hub
            .downstream_device_mut((Machine::UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT - 1) as usize)
            .expect("hub port 3 should contain a gamepad device");
        assert!(
            (gamepad.model() as &dyn Any).is::<aero_usb::hid::UsbHidGamepadHandle>(),
            "hub port 3 should host UsbHidGamepadHandle"
        );

        let consumer = hub
            .downstream_device_mut(
                (Machine::UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT - 1) as usize,
            )
            .expect("hub port 4 should contain a consumer-control device");
        assert!(
            (consumer.model() as &dyn Any).is::<aero_usb::hid::UsbHidConsumerControlHandle>(),
            "hub port 4 should host UsbHidConsumerControlHandle"
        );
    }

    // Hub port 4 is reserved for the synthetic consumer-control device.
    {
        let dummy = aero_usb::hid::UsbHidKeyboardHandle::new();
        let err = m
            .usb_attach_at_path(
                &[
                    Machine::UHCI_EXTERNAL_HUB_ROOT_PORT,
                    Machine::UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
                ],
                Box::new(dummy),
            )
            .expect_err("hub port 4 should be occupied by synthetic consumer-control");
        assert!(matches!(err, UsbHubAttachError::PortOccupied));
    }

    // Ensure the external hub has enough ports by attaching/detaching a dummy device behind it.
    //
    // Ports 1..=4 are reserved for the built-in synthetic devices (keyboard, mouse, gamepad,
    // consumer-control). Dynamic passthrough devices start at 5.
    {
        let dummy = aero_usb::hid::UsbHidKeyboardHandle::new();
        let passthrough_port = Machine::UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT + 1;
        m.usb_attach_at_path(
            &[Machine::UHCI_EXTERNAL_HUB_ROOT_PORT, passthrough_port],
            Box::new(dummy),
        )
        .expect("attaching behind external hub should succeed");
        m.usb_detach_at_path(&[Machine::UHCI_EXTERNAL_HUB_ROOT_PORT, passthrough_port])
            .expect("detaching behind external hub should succeed");
    }

    assert!(m.usb_hid_keyboard_handle().is_some());
    assert!(m.usb_hid_mouse_handle().is_some());
    assert!(m.usb_hid_gamepad_handle().is_some());
    assert!(m.usb_hid_consumer_control_handle().is_some());
}

#[test]
fn uhci_synthetic_usb_keyboard_pending_report_survives_snapshot_restore() {
    let cfg = synthetic_usb_hid_cfg();
    let mut src = Machine::new(cfg.clone()).unwrap();

    {
        let mut kbd = src
            .usb_hid_keyboard_handle()
            .expect("synthetic keyboard handle should be present");
        configure_keyboard_for_reports(&mut kbd);
    }

    // Press "A" (usage 0x04) and snapshot before the guest consumes the interrupt report.
    src.inject_usb_hid_keyboard_usage(0x04, true);
    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    {
        let mut kbd = restored
            .usb_hid_keyboard_handle()
            .expect("synthetic keyboard handle should be present after restore");
        configure_keyboard_for_reports(&mut kbd);
    }

    // After restore, the pending report should still be queued.
    expect_keyboard_report_contains(
        poll_keyboard_interrupt_in(&mut restored),
        0x04,
        "after snapshot restore",
    );

    // Verify post-restore injection still targets the guest-visible keyboard instance.
    restored.inject_usb_hid_keyboard_usage(0x04, false);
    match poll_keyboard_interrupt_in(&mut restored) {
        UsbInResult::Data(data) => assert_eq!(
            data,
            vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            "key release after restore should enqueue an updated report"
        ),
        other => panic!("expected interrupt report after key release, got {other:?}"),
    }
}

#[test]
fn uhci_synthetic_usb_hid_held_state_is_not_lost_before_configuration() {
    let cfg = synthetic_usb_hid_cfg();
    let mut m = Machine::new(cfg).unwrap();

    // Inject state *before* the guest configures the HID devices. The device models should keep
    // the latest state so the first interrupt-IN report after `SET_CONFIGURATION` reflects held
    // inputs ("held during enumeration" semantics).
    m.inject_usb_hid_keyboard_usage(0x04, true); // 'A'

    let gamepad_report = [0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00];
    inject_gamepad_report_bytes(&mut m, &gamepad_report);

    // Configure and verify the keyboard emits a report for the held key.
    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic keyboard handle should be present");
    assert!(!kbd.configured(), "keyboard should start unconfigured");
    configure_keyboard_for_reports(&mut kbd);
    expect_keyboard_report_contains(
        poll_keyboard_interrupt_in(&mut m),
        0x04,
        "after keyboard configuration",
    );

    // Configure and verify the gamepad emits a report for the held state.
    let mut gamepad = m
        .usb_hid_gamepad_handle()
        .expect("synthetic gamepad handle should be present");
    assert!(!gamepad.configured(), "gamepad should start unconfigured");
    configure_gamepad_for_reports(&mut gamepad);
    expect_gamepad_report(
        poll_gamepad_interrupt_in(&mut m),
        &gamepad_report,
        "after gamepad configuration",
    );
}

#[test]
fn uhci_synthetic_usb_hid_handles_survive_reset_and_snapshot_restore() {
    let cfg = synthetic_usb_hid_cfg();
    let mut m = Machine::new(cfg.clone()).unwrap();

    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic keyboard handle should be present");
    configure_keyboard_for_reports(&mut kbd);

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic mouse handle should be present");
    configure_mouse_for_reports(&mut mouse);

    let mut gamepad = m
        .usb_hid_gamepad_handle()
        .expect("synthetic gamepad handle should be present");
    configure_gamepad_for_reports(&mut gamepad);

    m.inject_usb_hid_keyboard_usage(0x04, true);
    expect_keyboard_report_contains(poll_keyboard_interrupt_in(&mut m), 0x04, "after injection");

    m.inject_usb_hid_mouse_move(10, 5);
    expect_mouse_report(
        poll_mouse_interrupt_in(&mut m),
        &[0, 10, 5, 0, 0],
        "after injection",
    );

    let gamepad_report = [0x03, 0x00, 0x02, 0x01, 0x02, 0x03, 0x04, 0x00];
    inject_gamepad_report_bytes(&mut m, &gamepad_report);
    expect_gamepad_report(
        poll_gamepad_interrupt_in(&mut m),
        &gamepad_report,
        "after injection",
    );

    let mut consumer = m
        .usb_hid_consumer_control_handle()
        .expect("synthetic consumer-control handle should be present");
    configure_consumer_for_reports(&mut consumer);
    m.inject_usb_hid_consumer_usage(0x00b5, true); // Scan Next Track
    expect_consumer_control_report(
        poll_consumer_interrupt_in(&mut m),
        0x00b5,
        "after injection",
    );

    m.reset();
    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic keyboard handle should persist across reset");
    configure_keyboard_for_reports(&mut kbd);

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic mouse handle should persist across reset");
    configure_mouse_for_reports(&mut mouse);

    let mut gamepad = m
        .usb_hid_gamepad_handle()
        .expect("synthetic gamepad handle should persist across reset");
    configure_gamepad_for_reports(&mut gamepad);

    // UHCI host controller reset preserves attached devices; use a different key so we always
    // trigger a report even if the previous key remains latched as pressed.
    m.inject_usb_hid_keyboard_usage(0x05, true);
    expect_keyboard_report_contains(poll_keyboard_interrupt_in(&mut m), 0x05, "after reset");

    m.inject_usb_hid_mouse_move(11, 6);
    expect_mouse_report(
        poll_mouse_interrupt_in(&mut m),
        &[0, 11, 6, 0, 0],
        "after reset",
    );

    let gamepad_report = [0x04, 0x00, 0x04, 0x05, 0x06, 0x07, 0x08, 0x00];
    inject_gamepad_report_bytes(&mut m, &gamepad_report);
    expect_gamepad_report(
        poll_gamepad_interrupt_in(&mut m),
        &gamepad_report,
        "after reset",
    );

    let mut consumer = m
        .usb_hid_consumer_control_handle()
        .expect("synthetic consumer-control handle should persist across reset");
    configure_consumer_for_reports(&mut consumer);
    m.inject_usb_hid_consumer_usage(0x00b6, true); // Scan Previous Track
    expect_consumer_control_report(poll_consumer_interrupt_in(&mut m), 0x00b6, "after reset");

    let snap = m.take_snapshot_full().unwrap();
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let mut kbd = restored
        .usb_hid_keyboard_handle()
        .expect("synthetic keyboard handle should persist across snapshot restore");
    configure_keyboard_for_reports(&mut kbd);

    let mut mouse = restored
        .usb_hid_mouse_handle()
        .expect("synthetic mouse handle should persist across snapshot restore");
    configure_mouse_for_reports(&mut mouse);

    let mut gamepad = restored
        .usb_hid_gamepad_handle()
        .expect("synthetic gamepad handle should persist across snapshot restore");
    configure_gamepad_for_reports(&mut gamepad);

    restored.inject_usb_hid_keyboard_usage(0x06, true);
    expect_keyboard_report_contains(
        poll_keyboard_interrupt_in(&mut restored),
        0x06,
        "after snapshot restore",
    );

    restored.inject_usb_hid_mouse_move(12, 7);
    expect_mouse_report(
        poll_mouse_interrupt_in(&mut restored),
        &[0, 12, 7, 0, 0],
        "after snapshot restore",
    );

    let gamepad_report = [0x08, 0x00, 0x00, 0x09, 0x0a, 0x0b, 0x0c, 0x00];
    inject_gamepad_report_bytes(&mut restored, &gamepad_report);
    expect_gamepad_report(
        poll_gamepad_interrupt_in(&mut restored),
        &gamepad_report,
        "after snapshot restore",
    );

    let mut consumer = restored
        .usb_hid_consumer_control_handle()
        .expect("synthetic consumer-control handle should persist across snapshot restore");
    configure_consumer_for_reports(&mut consumer);
    restored.inject_usb_hid_consumer_usage(0x00cd, true); // Play/Pause
    expect_consumer_control_report(
        poll_consumer_interrupt_in(&mut restored),
        0x00cd,
        "after snapshot restore",
    );
}

#[test]
fn inject_browser_key_routes_consumer_keys_to_synthetic_usb_consumer_control() {
    let mut m = Machine::new(synthetic_usb_hid_cfg()).unwrap();

    let mut consumer = m
        .usb_hid_consumer_control_handle()
        .expect("synthetic consumer-control handle should be present");
    configure_consumer_for_reports(&mut consumer);

    m.inject_browser_key("AudioVolumeUp", true);
    expect_consumer_control_report(
        poll_consumer_interrupt_in(&mut m),
        0x00e9,
        "after inject_browser_key keydown",
    );

    m.inject_browser_key("AudioVolumeUp", false);
    expect_consumer_control_report(
        poll_consumer_interrupt_in(&mut m),
        0,
        "after inject_browser_key keyup",
    );
}

#[test]
fn uhci_synthetic_usb_hid_does_not_overwrite_host_attached_root_port0_device() {
    let mut m = Machine::new(synthetic_usb_hid_cfg()).unwrap();

    // Replace the synthetic hub with a host-attached keyboard directly on root port 0.
    m.usb_detach_root(Machine::UHCI_EXTERNAL_HUB_ROOT_PORT)
        .expect("detaching synthetic hub");
    let dummy = aero_usb::hid::UsbHidKeyboardHandle::new();
    m.usb_attach_root(Machine::UHCI_EXTERNAL_HUB_ROOT_PORT, Box::new(dummy))
        .expect("attaching host device");

    // Reset should not overwrite the host-attached device.
    m.reset();
    let uhci = m.uhci().expect("UHCI device should exist");
    let uhci = uhci.borrow();
    let root = uhci.controller().hub();
    let dev0 = root
        .port_device(Machine::UHCI_EXTERNAL_HUB_ROOT_PORT as usize)
        .expect("UHCI root port 0 should remain occupied");
    assert!(
        dev0.as_hub().is_none(),
        "root port 0 should not be replaced with a synthetic hub"
    );
}
