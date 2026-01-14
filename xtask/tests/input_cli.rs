#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_lists_input_subcommand() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo xtask input"));
}

#[test]
fn input_help_mentions_flags_and_steps() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["input", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("aero-devices-input"))
        .stdout(predicate::str::contains("aero-usb"))
        .stdout(predicate::str::contains("ehci_snapshot_roundtrip"))
        .stdout(predicate::str::contains("aero-machine"))
        .stdout(predicate::str::contains("aero-wasm"))
        .stdout(predicate::str::contains("cargo test -p aero-wasm"))
        .stdout(predicate::str::contains("machine_input_injection"))
        .stdout(predicate::str::contains("machine_input_backends"))
        .stdout(predicate::str::contains("machine_i8042_snapshot_pending_bytes"))
        .stdout(predicate::str::contains("machine_virtio_input"))
        .stdout(predicate::str::contains("machine_uhci"))
        .stdout(predicate::str::contains("uhci_snapshot"))
        .stdout(predicate::str::contains("machine_uhci_snapshot_roundtrip"))
        .stdout(predicate::str::contains("uhci_usb_topology_api"))
        .stdout(predicate::str::contains("machine_usb_attach_at_path"))
        .stdout(predicate::str::contains("machine_ehci"))
        .stdout(predicate::str::contains("machine_usb2_companion_routing"))
        .stdout(predicate::str::contains("machine_uhci_synthetic_usb_hid"))
        .stdout(predicate::str::contains("machine_uhci_synthetic_hid"))
        .stdout(predicate::str::contains(
            "machine_uhci_synthetic_usb_hid_mouse_buttons",
        ))
        .stdout(predicate::str::contains(
            "machine_uhci_synthetic_usb_hid_gamepad",
        ))
        .stdout(predicate::str::contains(
            "machine_uhci_synthetic_usb_hid_reports",
        ))
        .stdout(predicate::str::contains("machine_xhci"))
        .stdout(predicate::str::contains("machine_xhci_snapshot"))
        .stdout(predicate::str::contains("xhci_snapshot"))
        .stdout(predicate::str::contains("machine_xhci_usb_attach_at_path"))
        .stdout(predicate::str::contains("usb_snapshot_host_state"))
        .stdout(predicate::str::contains("webusb_uhci_bridge"))
        .stdout(predicate::str::contains("xhci_webusb_bridge"))
        .stdout(predicate::str::contains("usb_guest_controller"))
        .stdout(predicate::str::contains("webusb_passthrough_runtime"))
        .stdout(predicate::str::contains(
            "src/usb/xhci_webusb_bridge.test.ts",
        ))
        .stdout(predicate::str::contains("xhci_webusb_passthrough_runtime"))
        .stdout(predicate::str::contains("uhci_webusb_root_port_rust_drift"))
        .stdout(predicate::str::contains("ehci_webusb_root_port_rust_drift"))
        .stdout(predicate::str::contains("xhci_webusb_root_port_rust_drift"))
        .stdout(predicate::str::contains("ehci_snapshot_roundtrip"))
        .stdout(predicate::str::contains("usb2_companion_routing"))
        .stdout(predicate::str::contains("webusb_passthrough_uhci"))
        .stdout(predicate::str::contains("xhci_controller_webusb_ep0"))
        .stdout(predicate::str::contains("xhci_enum_smoke"))
        .stdout(predicate::str::contains("xhci_webusb_passthrough"))
        .stdout(predicate::str::contains("--e2e"))
        .stdout(predicate::str::contains("--machine"))
        .stdout(predicate::str::contains("--wasm"))
        .stdout(predicate::str::contains("--with-wasm"))
        .stdout(predicate::str::contains("--rust-only"))
        .stdout(predicate::str::contains("--usb-all"));
}
