use assert_cmd::cargo::cargo_bin_cmd;
use std::env;
use tempfile::TempDir;

// Optional integration test:
// - skipped unless AERO_WIN7_ISO is set
// - uses signing-mode nointegritychecks to avoid requiring a cert in CI
#[test]
fn patch_and_verify_roundtrip() {
    let iso = match env::var_os("AERO_WIN7_ISO") {
        Some(v) => v,
        None => return,
    };
    let drivers = match env::var_os("AERO_WIN7_DRIVERS") {
        Some(v) => v,
        None => return,
    };

    let tmp = TempDir::new().unwrap();
    let out_iso = tmp.path().join("patched.iso");

    cargo_bin_cmd!("aero-win7-slipstream")
        .args([
            "patch-iso",
            "--input",
            iso.to_string_lossy().as_ref(),
            "--output",
            out_iso.to_string_lossy().as_ref(),
            "--drivers",
            drivers.to_string_lossy().as_ref(),
            "--signing-mode",
            "nointegritychecks",
            "--unattend",
            "drivers-only",
            "--backend",
            "auto",
        ])
        .assert()
        .success();

    cargo_bin_cmd!("aero-win7-slipstream")
        .args([
            "verify-iso",
            "--input",
            out_iso.to_string_lossy().as_ref(),
        ])
        .assert()
        .success();
}
