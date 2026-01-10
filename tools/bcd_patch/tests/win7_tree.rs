use std::fs;

use predicates::prelude::*;
use tempfile::tempdir;

fn write_synth_bcd(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, "testsigning=off\nnointegritychecks=off\n").unwrap();
}

fn read_file(path: &std::path::Path) -> String {
    fs::read_to_string(path).unwrap()
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
        .stdout(predicate::str::contains("summary: patched 3 store(s), missing 0 store(s)"));

    for path in [&bios, &uefi, &template] {
        let contents = read_file(path);
        assert!(contents.contains("testsigning=on"), "{path:?} not patched");
        assert!(contents.contains("nointegritychecks=on"), "{path:?} not patched");
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
        .stdout(predicate::str::contains("summary: patched 1 store(s), missing 2 store(s)"));

    let contents = read_file(&bios);
    assert!(contents.contains("testsigning=on"));
    assert!(contents.contains("nointegritychecks=on"));
}

#[test]
fn missing_stores_fail_in_strict_mode_without_patching() {
    let dir = tempdir().unwrap();

    let bios = dir.path().join("boot").join("BCD");
    write_synth_bcd(&bios);

    assert_cmd::cargo::cargo_bin_cmd!("bcd_patch")
        .args(["win7-tree", "--root"])
        .arg(dir.path())
        .arg("--strict")
        .assert()
        .failure();

    let contents = read_file(&bios);
    assert!(contents.contains("testsigning=off"));
    assert!(contents.contains("nointegritychecks=off"));
}
