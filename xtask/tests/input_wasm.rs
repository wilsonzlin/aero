#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Verify `cargo xtask input --wasm --rust-only` runs the wasm-pack bridge tests without requiring
/// `node_modules` (and therefore does not invoke `npm`).
///
/// This test stubs out `cargo`, `node`, `npm`, and `wasm-pack` via PATH so we can validate argv
/// wiring without running heavyweight suites.
#[test]
#[cfg(unix)]
fn input_wasm_runs_wasm_pack_without_node_modules() {
    said_runs_wasm_pack_without_node_modules().expect("test should succeed");
}

#[cfg(unix)]
fn said_runs_wasm_pack_without_node_modules() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let bin_dir = tmp.path().join("bin");
    fs::create_dir(&bin_dir)?;
    let log_path = tmp.path().join("argv.log");

    write_fake_argv_logger(&bin_dir.join("cargo"), "cargo")?;
    write_fake_argv_logger(&bin_dir.join("node"), "node")?;
    write_fake_argv_logger(&bin_dir.join("npm"), "npm")?;
    write_fake_argv_logger(&bin_dir.join("wasm-pack"), "wasm-pack")?;

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", bin_dir.display(), orig_path);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["input", "--wasm", "--rust-only"])
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .success();

    let log = fs::read_to_string(&log_path)?;
    let invocations = parse_invocations(&log);

    assert!(
        invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("node")),
        "expected a node version check invocation; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("npm")),
        "expected npm not to be invoked when --rust-only is set; invocations={invocations:?}"
    );

    let wasm_invocations: Vec<&Vec<String>> = invocations
        .iter()
        .filter(|argv| argv.first().map(|s| s.as_str()) == Some("wasm-pack"))
        .collect();
    assert!(
        !wasm_invocations.is_empty(),
        "expected wasm-pack to be invoked; invocations={invocations:?}"
    );

    let wasm_pack = wasm_invocations
        .into_iter()
        .find(|argv| argv.iter().any(|arg| arg == "crates/aero-wasm"))
        .ok_or("missing wasm-pack invocation")?;

    assert!(
        wasm_pack.iter().any(|arg| arg == "--node"),
        "expected wasm-pack test to use --node, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack.iter().any(|arg| arg == "--locked"),
        "expected wasm-pack to forward --locked, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack.iter().any(|arg| arg == "webusb_uhci_bridge"),
        "expected wasm-pack to include webusb_uhci_bridge, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack.iter().any(|arg| arg == "xhci_webusb_bridge"),
        "expected wasm-pack to include xhci_webusb_bridge, argv={wasm_pack:?}"
    );

    Ok(())
}

#[cfg(unix)]
fn write_fake_argv_logger(path: &Path, name: &str) -> std::io::Result<()> {
    let script = format!(
        r#"#!/bin/bash
set -euo pipefail
log="${{AERO_XTASK_TEST_LOG:?}}"
echo "{name}" >> "$log"
for arg in "$@"; do
  echo "$arg" >> "$log"
done
echo "__END__" >> "$log"
exit 0
"#
    );
    fs::write(path, script)?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn parse_invocations(log: &str) -> Vec<Vec<String>> {
    let mut invocations = Vec::new();
    let mut current = Vec::new();

    for line in log.lines() {
        if line == "__END__" {
            if !current.is_empty() {
                invocations.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(line.to_string());
    }

    if !current.is_empty() {
        invocations.push(current);
    }

    invocations
}
