#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_all_help_mentions_node_dir_aliases() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["test-all", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--node-dir"))
        .stdout(predicate::str::contains("--web-dir"))
        .stdout(predicate::str::contains("AERO_NODE_DIR"))
        .stdout(predicate::str::contains("AERO_WEB_DIR"))
        .stdout(predicate::str::contains("WEB_DIR"));
}

