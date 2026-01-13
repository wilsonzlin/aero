#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::process::Command;

#[test]
fn boots_fixture_and_prints_serial() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let disk = repo_root.join("tests/fixtures/boot/boot_vga_serial_8s.img");

    // Avoid relying on `CARGO_BIN_EXE_*` (Cargo does not guarantee it is set for all test
    // invocation modes). Use the workspace `target/` dir path instead.
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("target"));
    let exe_name = format!("aero-machine{}", std::env::consts::EXE_SUFFIX);
    let debug_exe = target_dir.join("debug").join(&exe_name);
    let release_exe = target_dir.join("release").join(&exe_name);
    let exe = if debug_exe.exists() {
        debug_exe
    } else if release_exe.exists() {
        release_exe
    } else {
        panic!(
            "expected aero-machine binary at {} or {}",
            debug_exe.display(),
            release_exe.display()
        );
    };

    let output = Command::new(exe)
        .args([
            "--disk",
            disk.to_str().expect("disk path should be UTF-8"),
            "--ram",
            "64",
            "--max-insts",
            "100000",
            "--serial-out",
            "stdout",
        ])
        .output()
        .expect("failed to run aero-machine CLI");

    assert!(
        output.status.success(),
        "aero-machine exited with {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = b"AERO!\r\n";
    assert!(
        output
            .stdout
            .windows(expected.len())
            .any(|w| w == expected),
        "stdout did not contain expected serial bytes.\nstdout:\n{:?}\nstderr:\n{}",
        output.stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}
