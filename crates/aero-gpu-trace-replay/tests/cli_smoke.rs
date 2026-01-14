#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

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

fn build_cmd_stream_with_unknown_opcode() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved1

    // Unknown opcode with 4-byte payload.
    bytes.extend_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
    bytes.extend_from_slice(&12u32.to_le_bytes()); // 8-byte hdr + 4-byte payload
    bytes.extend_from_slice(&[0, 1, 2, 3]);

    let size_bytes = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());
    bytes
}

#[test]
fn cli_decode_cmd_stream_json_succeeds_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cmd_path = dir.path().join("cmd_stream.bin");
    fs::write(&cmd_path, build_cmd_stream_with_unknown_opcode()).expect("write cmd stream");

    let output = Command::new(bin_path())
        .arg("decode-cmd-stream")
        .arg("--json")
        .arg(&cmd_path)
        .output()
        .expect("run decode-cmd-stream --json");
    assert!(output.status.success(), "stdout/stderr: {output:?}");

    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json output");
    let records = v["records"].as_array().expect("records array");
    assert!(
        records.iter().any(|r| {
            r["type"] == "packet"
                && r["opcode"].is_null()
                && r["opcode_u32"] == 0xDEAD_BEEF_u32
        }),
        "missing unknown opcode record: {v}"
    );
}

#[test]
fn cli_decode_cmd_stream_json_strict_fails_on_unknown_opcode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cmd_path = dir.path().join("cmd_stream.bin");
    fs::write(&cmd_path, build_cmd_stream_with_unknown_opcode()).expect("write cmd stream");

    let output = Command::new(bin_path())
        .arg("decode-cmd-stream")
        .arg("--json")
        .arg("--strict")
        .arg(&cmd_path)
        .output()
        .expect("run decode-cmd-stream --json --strict");
    assert!(!output.status.success(), "stdout/stderr: {output:?}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown opcode_id=0xDEADBEEF"), "{stderr}");
    assert!(stderr.contains("0x00000018"), "{stderr}");
}
