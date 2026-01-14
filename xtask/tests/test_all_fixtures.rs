#![cfg(not(target_arch = "wasm32"))]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

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
    let log = fs::read_to_string(path).expect("read fake cargo log");
    log.lines().map(|l| l.trim().to_string()).collect()
}

#[test]
fn test_all_fails_fast_when_fixtures_out_of_date() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("cargo.log");
    write_fake_cargo(tmp.path());

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["test-all", "--skip-wasm", "--skip-ts", "--skip-e2e"])
        .env("PATH", prepend_path(tmp.path()))
        .env("FAKE_CARGO_LOG", &log_path)
        .env("FAKE_CARGO_MODE", "fail_fixtures")
        .assert()
        .failure()
        // The fixture check itself should tell users how to regenerate.
        .stderr(predicate::str::contains("run `cargo xtask fixtures`"));

    let calls = read_log(&log_path);
    assert_eq!(
        calls,
        vec!["xtask fixtures --check"],
        "expected test-all to stop after the fixture check"
    );
}

#[test]
fn test_all_runs_fixture_checks_before_rust_fmt() {
    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("cargo.log");
    write_fake_cargo(tmp.path());

    // Force a failure after the fixture checks so we don't attempt to run real fmt/clippy/test.
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["test-all", "--skip-wasm", "--skip-ts", "--skip-e2e"])
        .env("PATH", prepend_path(tmp.path()))
        .env("FAKE_CARGO_LOG", &log_path)
        .env("FAKE_CARGO_MODE", "fail_fmt")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Rust: cargo fmt"));

    let calls = read_log(&log_path);
    assert!(
        calls.len() >= 3,
        "expected at least fixtures, bios-rom, and fmt invocations; got: {calls:?}"
    );
    assert_eq!(calls[0], "xtask fixtures --check");
    assert_eq!(calls[1], "xtask bios-rom --check");
    assert!(
        calls.iter().any(|c| c == "fmt --all -- --check"),
        "expected fmt check to run after fixture validation; got: {calls:?}"
    );
}
