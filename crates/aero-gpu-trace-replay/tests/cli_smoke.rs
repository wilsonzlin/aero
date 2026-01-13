#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/aerogpu_cmd_triangle.aerogputrace")
}

fn bin_path() -> PathBuf {
    option_env!("CARGO_BIN_EXE_aero-gpu-trace-replay")
        .or(option_env!("CARGO_BIN_EXE_aero_gpu_trace_replay"))
        .map(PathBuf::from)
        .expect("Cargo should set CARGO_BIN_EXE_* for integration tests")
}

#[test]
fn cli_outputs_stable_sha256_line_format() {
    let output = Command::new(bin_path())
        .arg(fixture_path())
        .output()
        .expect("run aero-gpu-trace-replay");
    assert!(output.status.success(), "stdout/stderr: {output:?}");

    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    let line = stdout.trim_end();

    assert!(
        line.starts_with("frame 0: 64x64 sha256="),
        "unexpected stdout: {line:?}"
    );
    let hash = line
        .strip_prefix("frame 0: 64x64 sha256=")
        .expect("prefix checked");
    assert_eq!(hash.len(), 64, "unexpected hash length: {hash:?}");
    assert!(
        hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        "unexpected hash characters: {hash:?}"
    );
}

#[test]
fn cli_dump_png_writes_a_png_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = Command::new(bin_path())
        .arg("--dump-png")
        .arg(dir.path())
        .arg(fixture_path())
        .output()
        .expect("run aero-gpu-trace-replay --dump-png");
    assert!(output.status.success(), "stdout/stderr: {output:?}");

    let png_path = dir.path().join("frame_0.png");
    let bytes = fs::read(&png_path).expect("read dumped png");
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "file did not have PNG signature: {png_path:?}"
    );
}
