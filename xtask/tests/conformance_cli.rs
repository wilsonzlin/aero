#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn conformance_help_prints_usage() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["conformance", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo xtask conformance"));
}

#[test]
fn top_level_help_mentions_conformance() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("conformance"));
}

