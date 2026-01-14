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
        .stdout(predicate::str::contains("--e2e"));
}
