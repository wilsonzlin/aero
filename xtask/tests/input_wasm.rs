#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Verify `cargo xtask input` wiring for sandbox-friendly modes.
///
/// These tests stub out `cargo`, `node`, `npm`, and `wasm-pack` via PATH so we can validate argv
/// wiring without running heavyweight suites.
///
/// Each test writes an argv log to `AERO_XTASK_TEST_LOG`, then inspects which tools were invoked.
///
#[test]
#[cfg(unix)]
fn input_wasm_runs_wasm_pack_without_node_modules() {
    said_runs_wasm_pack_without_node_modules().expect("test should succeed");
}

/// Verify `cargo xtask input --rust-only` does not invoke Node/npm/wasm-pack (so it can run on a
/// pure-Rust machine without `node_modules`).
#[test]
#[cfg(unix)]
fn input_rust_only_skips_node_and_npm() {
    said_rust_only_skips_node_and_npm().expect("test should succeed");
}

/// Verify `--usb-all` removes `--test` filters for the `aero-usb` invocation.
#[test]
#[cfg(unix)]
fn input_usb_all_runs_full_aero_usb_suite() {
    said_usb_all_runs_full_aero_usb_suite().expect("test should succeed");
}

/// Verify `cargo xtask input --machine --rust-only` invokes the `aero-machine` USB wiring tests and
/// still does not require Node/npm.
#[test]
#[cfg(unix)]
fn input_machine_rust_only_runs_machine_tests() {
    said_machine_rust_only_runs_machine_tests().expect("test should succeed");
}

/// Verify `cargo xtask input --with-wasm --rust-only` runs the host-side aero-wasm integration
/// tests (no wasm-pack) without requiring Node/npm.
#[test]
#[cfg(unix)]
fn input_with_wasm_rust_only_runs_aero_wasm_integration_tests() {
    said_with_wasm_rust_only_runs_aero_wasm_integration_tests().expect("test should succeed");
}

/// Verify `cargo xtask input --wasm --rust-only` reports a helpful error when `wasm-pack` is missing.
#[test]
#[cfg(unix)]
fn input_wasm_reports_missing_wasm_pack() {
    said_wasm_reports_missing_wasm_pack().expect("test should succeed");
}

#[cfg(unix)]
fn said_runs_wasm_pack_without_node_modules() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let bin_dir = tmp.path().join("bin");
    fs::create_dir(&bin_dir)?;
    let log_path = tmp.path().join("argv.log");

    write_fake_argv_logger(&bin_dir.join("cargo"), "cargo")?;
    write_fake_node_version_checker(&bin_dir.join("node"))?;
    write_fake_argv_logger(&bin_dir.join("npm"), "npm")?;
    write_fake_argv_logger(&bin_dir.join("wasm-pack"), "wasm-pack")?;

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", bin_dir.display(), orig_path);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["input", "--wasm", "--rust-only"])
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped npm + Playwright"));

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
    for expected in [
        "uhci_controller_topology",
        "uhci_runtime_webusb_drain_actions",
        "uhci_runtime_topology",
        "uhci_runtime_external_hub",
        "uhci_runtime_snapshot_roundtrip",
    ] {
        assert!(
            wasm_pack.iter().any(|arg| arg == expected),
            "expected wasm-pack to include {expected}, argv={wasm_pack:?}"
        );
    }
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "ehci_controller_bridge_snapshot_roundtrip"),
        "expected wasm-pack to include ehci_controller_bridge_snapshot_roundtrip, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "ehci_controller_topology"),
        "expected wasm-pack to include ehci_controller_topology, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "webusb_ehci_passthrough_harness"),
        "expected wasm-pack to include webusb_ehci_passthrough_harness, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack.iter().any(|arg| arg == "xhci_webusb_bridge"),
        "expected wasm-pack to include xhci_webusb_bridge, argv={wasm_pack:?}"
    );
    for expected in [
        "xhci_controller_bridge",
        "xhci_controller_bridge_topology",
        "xhci_controller_bridge_webusb",
        "xhci_controller_topology",
        "xhci_bme_event_ring",
    ] {
        assert!(
            wasm_pack.iter().any(|arg| arg == expected),
            "expected wasm-pack to include {expected}, argv={wasm_pack:?}"
        );
    }
    assert!(
        wasm_pack.iter().any(|arg| arg == "xhci_webusb_snapshot"),
        "expected wasm-pack to include xhci_webusb_snapshot, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "usb_bridge_snapshot_roundtrip"),
        "expected wasm-pack to include usb_bridge_snapshot_roundtrip, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack.iter().any(|arg| arg == "usb_snapshot"),
        "expected wasm-pack to include usb_snapshot, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "machine_input_injection_wasm"),
        "expected wasm-pack to include machine_input_injection_wasm, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack.iter().any(|arg| arg == "wasm_machine_ps2_mouse"),
        "expected wasm-pack to include wasm_machine_ps2_mouse, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "usb_hid_bridge_keyboard_reports_wasm"),
        "expected wasm-pack to include usb_hid_bridge_keyboard_reports_wasm, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "usb_hid_bridge_mouse_reports_wasm"),
        "expected wasm-pack to include usb_hid_bridge_mouse_reports_wasm, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "usb_hid_bridge_consumer_reports_wasm"),
        "expected wasm-pack to include usb_hid_bridge_consumer_reports_wasm, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "webhid_interrupt_out_policy_wasm"),
        "expected wasm-pack to include webhid_interrupt_out_policy_wasm, argv={wasm_pack:?}"
    );
    assert!(
        wasm_pack
            .iter()
            .any(|arg| arg == "webhid_report_descriptor_synthesis_wasm"),
        "expected wasm-pack to include webhid_report_descriptor_synthesis_wasm, argv={wasm_pack:?}"
    );

    Ok(())
}

#[cfg(unix)]
fn said_rust_only_skips_node_and_npm() -> Result<(), Box<dyn std::error::Error>> {
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
        .args(["input", "--rust-only"])
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .success();

    let log = fs::read_to_string(&log_path)?;
    let invocations = parse_invocations(&log);

    assert!(
        invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("cargo")),
        "expected cargo to be invoked; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("node")),
        "expected node not to be invoked when --rust-only is set; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("npm")),
        "expected npm not to be invoked when --rust-only is set; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("wasm-pack")),
        "expected wasm-pack not to be invoked without --wasm; invocations={invocations:?}"
    );

    let cargo_usb = invocations
        .iter()
        .find(|argv| argv.iter().any(|arg| arg == "aero-usb"));
    let Some(cargo_usb) = cargo_usb else {
        return Err("missing cargo invocation for aero-usb".into());
    };
    assert!(
        cargo_usb.iter().any(|arg| arg == "--test"),
        "expected default input run to filter aero-usb tests (use --usb-all to remove); argv={cargo_usb:?}"
    );
    for expected in [
        "uhci",
        "ehci",
        "xhci_enum_smoke",
        "webusb_passthrough_uhci",
        "xhci_stop_endpoint_unschedules",
        "xhci_webusb_passthrough",
    ] {
        assert!(
            cargo_usb.iter().any(|arg| arg == expected),
            "expected `{expected}` to be part of the focused aero-usb test list; argv={cargo_usb:?}"
        );
    }

    Ok(())
}

#[cfg(unix)]
fn said_usb_all_runs_full_aero_usb_suite() -> Result<(), Box<dyn std::error::Error>> {
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
        .args(["input", "--rust-only", "--usb-all"])
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .success();

    let log = fs::read_to_string(&log_path)?;
    let invocations = parse_invocations(&log);

    let cargo_usb = invocations
        .iter()
        .find(|argv| argv.iter().any(|arg| arg == "aero-usb"));
    let Some(cargo_usb) = cargo_usb else {
        return Err("missing cargo invocation for aero-usb".into());
    };
    assert!(
        !cargo_usb.iter().any(|arg| arg == "--test"),
        "expected --usb-all to run the full aero-usb suite (no --test filters), argv={cargo_usb:?}"
    );

    Ok(())
}

#[cfg(unix)]
fn said_machine_rust_only_runs_machine_tests() -> Result<(), Box<dyn std::error::Error>> {
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
        .args(["input", "--machine", "--rust-only"])
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .success();

    let log = fs::read_to_string(&log_path)?;
    let invocations = parse_invocations(&log);

    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("node")),
        "expected node not to be invoked when --rust-only is set; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("npm")),
        "expected npm not to be invoked when --rust-only is set; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("wasm-pack")),
        "expected wasm-pack not to be invoked without --wasm; invocations={invocations:?}"
    );

    let cargo_machine = invocations
        .iter()
        .find(|argv| argv.iter().any(|arg| arg == "aero-machine"));
    let Some(cargo_machine) = cargo_machine else {
        return Err("missing cargo invocation for aero-machine".into());
    };

    // Keep this in sync with the `--machine` command in `xtask/src/cmd_input.rs`.
    for expected in [
        "--lib",
        "--locked",
        "machine_i8042_snapshot_pending_bytes",
        "machine_virtio_input",
        "machine_uhci",
        "uhci_snapshot",
        "machine_uhci_snapshot_roundtrip",
        "uhci_usb_topology_api",
        "machine_usb_attach_at_path",
        "machine_ehci",
        "machine_usb2_companion_routing",
        "machine_uhci_synthetic_usb_hid",
        "machine_uhci_synthetic_hid",
        "machine_uhci_synthetic_usb_hid_mouse_buttons",
        "machine_uhci_synthetic_usb_hid_gamepad",
        "machine_uhci_synthetic_usb_hid_reports",
        "machine_xhci",
        "machine_xhci_snapshot",
        "xhci_snapshot",
        "machine_xhci_usb_attach_at_path",
        "usb_snapshot_host_state",
    ] {
        assert!(
            cargo_machine.iter().any(|arg| arg == expected),
            "expected `{expected}` in aero-machine argv; argv={cargo_machine:?}"
        );
    }

    Ok(())
}

#[cfg(unix)]
fn said_with_wasm_rust_only_runs_aero_wasm_integration_tests(
) -> Result<(), Box<dyn std::error::Error>> {
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
        .args(["input", "--with-wasm", "--rust-only"])
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .success();

    let log = fs::read_to_string(&log_path)?;
    let invocations = parse_invocations(&log);

    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("node")),
        "expected node not to be invoked when --rust-only is set; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("npm")),
        "expected npm not to be invoked when --rust-only is set; invocations={invocations:?}"
    );
    assert!(
        !invocations
            .iter()
            .any(|argv| argv.first().map(|s| s.as_str()) == Some("wasm-pack")),
        "expected wasm-pack not to be invoked without --wasm; invocations={invocations:?}"
    );

    let cargo_wasm = invocations
        .iter()
        .find(|argv| argv.iter().any(|arg| arg == "aero-wasm"));
    let Some(cargo_wasm) = cargo_wasm else {
        return Err("missing cargo invocation for aero-wasm".into());
    };
    for expected in [
        "--locked",
        "machine_input_injection",
        "machine_input_backends",
        "machine_defaults_usb_hid",
        "webhid_report_descriptor_synthesis",
        "machine_virtio_input",
    ] {
        assert!(
            cargo_wasm.iter().any(|arg| arg == expected),
            "expected `{expected}` in aero-wasm argv; argv={cargo_wasm:?}"
        );
    }

    Ok(())
}

#[cfg(unix)]
fn said_wasm_reports_missing_wasm_pack() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let bin_dir = tmp.path().join("bin");
    fs::create_dir(&bin_dir)?;
    let log_path = tmp.path().join("argv.log");

    write_fake_argv_logger(&bin_dir.join("cargo"), "cargo")?;
    write_fake_argv_logger(&bin_dir.join("node"), "node")?;

    // Do not provide a wasm-pack stub, and avoid inheriting the real PATH, so wasm-pack is
    // guaranteed to be missing for this test even on developer machines.
    let path = bin_dir.display().to_string();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["input", "--wasm", "--rust-only"])
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains(
            "missing required command: wasm-pack",
        ))
        .stderr(predicates::str::contains("Install wasm-pack"));

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

#[cfg(unix)]
fn write_fake_node_version_checker(path: &Path) -> std::io::Result<()> {
    let script = r#"#!/bin/bash
set -euo pipefail
log="${AERO_XTASK_TEST_LOG:?}"
echo "node" >> "$log"
for arg in "$@"; do
  echo "$arg" >> "$log"
done
echo "__END__" >> "$log"

if [[ "${AERO_ENFORCE_NODE_MAJOR-}" != "1" ]]; then
  echo "expected AERO_ENFORCE_NODE_MAJOR=1 when running cargo xtask input --wasm" >&2
  exit 1
fi

exit 0
"#;

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
