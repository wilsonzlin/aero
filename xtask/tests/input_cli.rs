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
        .stdout(predicate::str::contains("aero-machine"))
        .stdout(predicate::str::contains("aero-wasm"))
        .stdout(predicate::str::contains("machine_uhci"))
        .stdout(predicate::str::contains("machine_uhci_synthetic_usb_hid"))
        .stdout(predicate::str::contains("machine_uhci_synthetic_usb_hid_reports"))
        .stdout(predicate::str::contains("machine_xhci"))
        .stdout(predicate::str::contains("xhci_snapshot"))
        .stdout(predicate::str::contains("machine_xhci_usb_attach_at_path"))
        .stdout(predicate::str::contains("webusb_uhci_bridge"))
        .stdout(predicate::str::contains("xhci_webusb_bridge"))
        .stdout(predicate::str::contains("usb_guest_controller"))
        .stdout(predicate::str::contains("ehci_webusb_root_port_rust_drift"))
        .stdout(predicate::str::contains("xhci_webusb_root_port_rust_drift"))
        .stdout(predicate::str::contains("xhci_enum_smoke"))
        .stdout(predicate::str::contains("--e2e"))
        .stdout(predicate::str::contains("--machine"))
        .stdout(predicate::str::contains("--wasm"))
        .stdout(predicate::str::contains("--with-wasm"))
        .stdout(predicate::str::contains("--rust-only"))
        .stdout(predicate::str::contains("--usb-all"));
}
