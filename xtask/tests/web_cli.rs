#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn web_help_mentions_node_dir_aliases() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["web", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--node-dir"))
        .stdout(predicate::str::contains("--web-dir"))
        .stdout(predicate::str::contains("AERO_NODE_DIR"))
        .stdout(predicate::str::contains("AERO_WEB_DIR"))
        .stdout(predicate::str::contains("WEB_DIR"));
}

#[test]
fn web_rejects_empty_node_dir_equals() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["web", "dev", "--node-dir="])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("--node-dir requires a value"));
}
