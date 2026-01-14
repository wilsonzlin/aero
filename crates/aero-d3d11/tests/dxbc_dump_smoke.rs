#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::process::Command;

#[test]
fn dxbc_dump_runs_on_gs_fixture() {
    let bin = env!("CARGO_BIN_EXE_dxbc_dump");

    let mut fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fixture.push("tests/fixtures/gs_emit_cut.dxbc");

    let output = Command::new(bin)
        .arg(&fixture)
        .arg("--head")
        .arg("4")
        .output()
        .expect("run dxbc_dump");

    assert!(
        output.status.success(),
        "dxbc_dump failed (status={:?})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Basic sanity assertions that ensure the tool prints the core information needed for
    // GS opcode discovery.
    assert!(stdout.contains("shader chunk: SHDR"));
    assert!(stdout.contains("stage=Geometry"));
    assert!(stdout.contains("(emit)"));
    assert!(stdout.contains("(cut)"));
}

