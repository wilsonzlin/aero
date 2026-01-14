#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::usb::uhci::regs;
use aero_machine::{Machine, MachineConfig};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

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
    u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
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

fn poll_keyboard_interrupt_in(m: &mut Machine) -> UsbInResult {
    let uhci = m.uhci().expect("UHCI device should exist");
    let mut uhci = uhci.borrow_mut();
    let root = uhci.controller_mut().hub_mut();
    let mut dev0 = root
        .port_device_mut(0)
        .expect("UHCI root port 0 should have an external hub attached");
    let hub = dev0.as_hub_mut().expect("root port 0 device should be a hub");
    let keyboard = hub
        .downstream_device_mut(0)
        .expect("hub port 1 should contain a keyboard device");
    keyboard.model_mut().handle_interrupt_in(0x81)
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

    let base = uhci_io_base(&m);
    let portsc1 = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_ne!(portsc1 & 0x0001, 0, "PORTSC1.CCS should be set");
    assert_ne!(portsc1 & 0x0002, 0, "PORTSC1.CSC should be set");

    // Root port 0 should contain an external hub with the runtime port count.
    {
        let uhci = m.uhci().expect("UHCI device should exist");
        let uhci = uhci.borrow();
        let root = uhci.controller().hub();
        let dev0 = root
            .port_device(0)
            .expect("UHCI root port 0 should have an external hub attached");
        let hub = dev0.as_hub().expect("root port 0 device should be a hub");
        assert_eq!(
            hub.num_ports(),
            16,
            "external hub port count should match web runtime"
        );
    }

    // Ensure the external hub has enough ports by attaching/detaching a dummy device behind it.
    {
        let dummy = aero_usb::hid::UsbHidKeyboardHandle::new();
        // Ports 1-4 are occupied by the synthetic HID devices; use a higher-numbered port.
        m.usb_attach_at_path(&[0, 5], Box::new(dummy))
            .expect("attaching behind external hub should succeed");
        m.usb_detach_at_path(&[0, 5])
            .expect("detaching behind external hub should succeed");
    }

    assert!(m.usb_hid_keyboard_handle().is_some());
    assert!(m.usb_hid_mouse_handle().is_some());
    assert!(m.usb_hid_gamepad_handle().is_some());
}

#[test]
fn uhci_synthetic_usb_hid_handles_survive_reset_and_snapshot_restore() {
    let cfg = synthetic_usb_hid_cfg();
    let mut m = Machine::new(cfg.clone()).unwrap();

    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic keyboard handle should be present");
    configure_keyboard_for_reports(&mut kbd);

    m.inject_usb_hid_keyboard_usage(0x04, true);
    assert!(
        matches!(poll_keyboard_interrupt_in(&mut m), UsbInResult::Data(_)),
        "expected keyboard interrupt IN to return data after injection"
    );

    m.reset();
    let mut kbd = m
        .usb_hid_keyboard_handle()
        .expect("synthetic keyboard handle should persist across reset");
    configure_keyboard_for_reports(&mut kbd);

    // UHCI host controller reset preserves attached devices; use a different key so we always
    // trigger a report even if the previous key remains latched as pressed.
    m.inject_usb_hid_keyboard_usage(0x05, true);
    assert!(
        matches!(poll_keyboard_interrupt_in(&mut m), UsbInResult::Data(_)),
        "expected keyboard interrupt IN to return data after reset"
    );

    let snap = m.take_snapshot_full().unwrap();
    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let mut kbd = restored
        .usb_hid_keyboard_handle()
        .expect("synthetic keyboard handle should persist across snapshot restore");
    configure_keyboard_for_reports(&mut kbd);

    restored.inject_usb_hid_keyboard_usage(0x06, true);
    assert!(
        matches!(
            poll_keyboard_interrupt_in(&mut restored),
            UsbInResult::Data(_)
        ),
        "expected keyboard interrupt IN to return data after snapshot restore"
    );
}

#[test]
fn uhci_synthetic_usb_hid_does_not_overwrite_host_attached_root_port0_device() {
    let mut m = Machine::new(synthetic_usb_hid_cfg()).unwrap();

    // Replace the synthetic hub with a host-attached keyboard directly on root port 0.
    m.usb_detach_root(0).expect("detaching synthetic hub");
    let dummy = aero_usb::hid::UsbHidKeyboardHandle::new();
    m.usb_attach_root(0, Box::new(dummy))
        .expect("attaching host device");

    // Reset should not overwrite the host-attached device.
    m.reset();
    let uhci = m.uhci().expect("UHCI device should exist");
    let uhci = uhci.borrow();
    let root = uhci.controller().hub();
    let dev0 = root
        .port_device(0)
        .expect("UHCI root port 0 should remain occupied");
    assert!(
        dev0.as_hub().is_none(),
        "root port 0 should not be replaced with a synthetic hub"
    );
}
