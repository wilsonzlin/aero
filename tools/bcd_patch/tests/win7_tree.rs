mod common;

use std::fs;

use common::*;
use predicates::prelude::*;
use regf::RegistryHive;
use tempfile::tempdir;

fn write_synth_bcd(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, build_minimal_bcd_hive(false)).unwrap();
}

fn assert_store_patched(path: &std::path::Path) {
    let hive = RegistryHive::from_file(path).unwrap();
    assert_boolean_element(
        &hive,
        OBJ_GLOBALSETTINGS,
        ELEM_DISABLE_INTEGRITY_CHECKS,
        true,
    );
    assert_boolean_element(
        &hive,
        OBJ_GLOBALSETTINGS,
        ELEM_ALLOW_PRERELEASE_SIGNATURES,
        true,
    );
}

#[test]
fn patches_all_present_stores_case_insensitively() {
    let dir = tempdir().unwrap();

    let bios = dir.path().join("BOOT").join("bcd");
    let uefi = dir
        .path()
        .join("eFi")
        .join("Microsoft")
        .join("Boot")
        .join("BCD");
    let template = dir
        .path()
        .join("windows")
        .join("SYSTEM32")
        .join("config")
        .join("BCD-Template");

    write_synth_bcd(&bios);
    write_synth_bcd(&uefi);
    write_synth_bcd(&template);

    assert_cmd::cargo::cargo_bin_cmd!("bcd_patch")
        .args(["win7-tree", "--root"])
        .arg(dir.path())
        .args(["--testsigning", "on", "--nointegritychecks", "on"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "summary: patched 3 store(s), missing 0 store(s)",
        ));

    for path in [&bios, &uefi, &template] {
        assert_store_patched(path);
    }
}

#[test]
fn missing_stores_warn_in_non_strict_mode() {
    let dir = tempdir().unwrap();

    let bios = dir.path().join("boot").join("BCD");
    write_synth_bcd(&bios);

    assert_cmd::cargo::cargo_bin_cmd!("bcd_patch")
        .args(["win7-tree", "--root"])
        .arg(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("warning: missing BCD store"))
        .stdout(predicate::str::contains(
            "summary: patched 1 store(s), missing 2 store(s)",
        ));

    assert_store_patched(&bios);
}

#[test]
fn missing_stores_fail_in_strict_mode_without_patching() {
    let dir = tempdir().unwrap();

    let bios = dir.path().join("boot").join("BCD");
    write_synth_bcd(&bios);
    let before = fs::read(&bios).unwrap();

    assert_cmd::cargo::cargo_bin_cmd!("bcd_patch")
        .args(["win7-tree", "--root"])
        .arg(dir.path())
        .arg("--strict")
        .assert()
        .failure();

    let after = fs::read(&bios).unwrap();
    assert_eq!(before, after);
}
