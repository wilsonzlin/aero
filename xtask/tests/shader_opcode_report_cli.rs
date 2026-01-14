#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use predicates::prelude::*;

#[cfg(feature = "shader-opcode-report")]
use std::io::Write;

#[test]
#[cfg(feature = "shader-opcode-report")]
fn reports_fixture_dxbc() {
    let fixture = format!(
        "{}/../crates/aero-d3d9/tests/fixtures/dxbc/ps_2_0_sample.dxbc",
        env!("CARGO_MANIFEST_DIR")
    );

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("shader-opcode-report")
        .arg(&fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("shader: ps_2_0"))
        .stdout(predicate::str::contains("0x0042 tex"));
}

#[test]
#[cfg(not(feature = "shader-opcode-report"))]
fn reports_fixture_dxbc_requires_feature() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("shader-opcode-report")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires building `xtask` with the `shader-opcode-report` feature",
        ));
}

#[test]
#[cfg(feature = "shader-opcode-report")]
fn deny_unsupported_exits_nonzero() {
    let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");

    // ps_2_0 with a single unknown opcode, then `end`.
    let tokens: [u32; 3] = [0xFFFF_0200, 0x0000_1234, 0x0000_FFFF];
    for t in tokens {
        tmp.write_all(&t.to_le_bytes()).unwrap();
    }

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("shader-opcode-report")
        .arg("--deny-unsupported")
        .arg(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsupported opcodes found"));
}

#[test]
#[cfg(not(feature = "shader-opcode-report"))]
fn deny_unsupported_requires_feature() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["shader-opcode-report", "--deny-unsupported"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires building `xtask` with the `shader-opcode-report` feature",
        ));
}
