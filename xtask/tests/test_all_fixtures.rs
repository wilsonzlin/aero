#![cfg(not(target_arch = "wasm32"))]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

static REPO_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write_fake_cargo(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join("cargo.cmd");
        fs::write(
            &path,
            r#"@echo off
setlocal enabledelayedexpansion

if "%FAKE_CARGO_LOG%"=="" (
  echo FAKE_CARGO_LOG must be set 1>&2
  exit /b 99
)

echo %*>> "%FAKE_CARGO_LOG%"

if "%FAKE_CARGO_MODE%"=="fail_fixtures" (
  if "%1"=="xtask" if "%2"=="fixtures" if "%3"=="--check" (
    echo error: tests/fixtures/boot/boot_vga_serial.bin is out of date (run `cargo xtask fixtures`) 1>&2
    exit /b 1
  )
)

if "%FAKE_CARGO_MODE%"=="fail_fmt" (
  if "%1"=="fmt" (
    echo error: fake fmt failure 1>&2
    exit /b 2
  )
)

exit /b 0
"#,
        )
        .expect("write fake cargo.cmd");
        return path;
    }

    let path = dir.join("cargo");
    fs::write(
        &path,
        r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${FAKE_CARGO_LOG:-}" ]]; then
  echo "FAKE_CARGO_LOG must be set" >&2
  exit 99
fi

echo "$@" >> "$FAKE_CARGO_LOG"

if [[ "${FAKE_CARGO_MODE:-}" == "fail_fixtures" ]]; then
  if [[ "${1:-}" == "xtask" && "${2:-}" == "fixtures" && "${3:-}" == "--check" ]]; then
    echo "error: tests/fixtures/boot/boot_vga_serial.bin is out of date (run \`cargo xtask fixtures\`)" >&2
    exit 1
  fi
fi

if [[ "${FAKE_CARGO_MODE:-}" == "fail_fmt" ]]; then
  if [[ "${1:-}" == "fmt" ]]; then
    echo "error: fake fmt failure" >&2
    exit 2
  fi
fi

exit 0
"#,
    )
    .expect("write fake cargo");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).expect("stat fake cargo").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod fake cargo");
    }

    path
}

fn prepend_path(dir: &Path) -> OsString {
    let mut out = OsString::new();
    out.push(dir.as_os_str());
    out.push(if cfg!(windows) { ";" } else { ":" });
    out.push(std::env::var_os("PATH").unwrap_or_default());
    out
}

fn read_log(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(log) => log.lines().map(|l| l.trim().to_string()).collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => panic!("read fake cargo log: {err}"),
    }
}

struct FileRestore {
    path: PathBuf,
    original: Vec<u8>,
}

impl Drop for FileRestore {
    fn drop(&mut self) {
        let _ = fs::write(&self.path, &self.original);
    }
}

fn corrupt_fixture_bytes(rel_path: &str) -> FileRestore {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest dir has parent")
        .to_path_buf();
    let path = repo_root.join(rel_path);
    let original = fs::read(&path).unwrap_or_else(|_| panic!("read fixture {path:?}"));
    let mut mutated = original.clone();
    // Flip a bit so `xtask fixtures --check` will detect drift.
    if mutated.is_empty() {
        mutated.push(0);
    } else {
        mutated[0] ^= 0x01;
    }
    fs::write(&path, &mutated).unwrap_or_else(|_| panic!("write fixture {path:?}"));
    FileRestore { path, original }
}

#[test]
fn test_all_fails_fast_when_fixtures_out_of_date() {
    let _guard = REPO_LOCK.lock().unwrap();
    let _restore = corrupt_fixture_bytes("tests/fixtures/boot/boot_vga_serial.bin");

    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("cargo.log");
    write_fake_cargo(tmp.path());

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["test-all", "--skip-wasm", "--skip-ts", "--skip-e2e"])
        .env("PATH", prepend_path(tmp.path()))
        .env("FAKE_CARGO_LOG", &log_path)
        .output()
        .expect("run xtask test-all");

    assert!(
        !output.status.success(),
        "expected test-all to fail when fixtures are out of date"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The fixture check itself should tell users how to regenerate.
    assert!(
        stderr.contains("run `cargo xtask fixtures`"),
        "stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("boot_vga_serial.bin"),
        "stderr should mention the drifted fixture; got:\n{stderr}"
    );

    // `test-all` should stop after the fixture check and not invoke any `cargo` steps.
    let calls = read_log(&log_path);
    assert!(
        calls.is_empty(),
        "expected no cargo invocations; got: {calls:?}"
    );
}

#[test]
fn test_all_runs_fixture_checks_before_rust_fmt() {
    let _guard = REPO_LOCK.lock().unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("cargo.log");
    write_fake_cargo(tmp.path());

    // Force a failure after the fixture checks so we don't attempt to run real fmt/clippy/test.
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["test-all", "--skip-wasm", "--skip-ts", "--skip-e2e"])
        .env("PATH", prepend_path(tmp.path()))
        .env("FAKE_CARGO_LOG", &log_path)
        .env("FAKE_CARGO_MODE", "fail_fmt")
        .output()
        .expect("run xtask test-all");

    assert!(
        !output.status.success(),
        "expected test-all to fail at the fake fmt step"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Rust: cargo fmt"),
        "stderr should mention the fmt step; got:\n{stderr}"
    );

    let fixtures_idx = stdout
        .find("==> Fixtures: cargo xtask fixtures --check")
        .expect("stdout should include fixtures check step");
    let bios_rom_idx = stdout
        .find("==> BIOS ROM: cargo xtask bios-rom --check")
        .expect("stdout should include bios-rom check step");
    let fmt_idx = stdout
        .find("==> Rust: cargo fmt --all -- --check")
        .expect("stdout should include rust fmt step");
    assert!(
        fixtures_idx < bios_rom_idx && bios_rom_idx < fmt_idx,
        "expected fixtures -> bios-rom -> fmt order; stdout:\n{stdout}"
    );

    let calls = read_log(&log_path);
    assert_eq!(
        calls,
        vec!["fmt --all -- --check"],
        "expected only the fmt invocation (the fake cargo fails there); got: {calls:?}"
    );
}
