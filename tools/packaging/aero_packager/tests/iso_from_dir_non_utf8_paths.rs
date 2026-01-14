#![cfg(target_os = "linux")]

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt as _;

#[test]
fn iso_from_dir_fails_on_non_utf8_paths() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();

    fs::write(root.join("ok.txt"), b"ok\n")?;

    // Create a file with a non-UTF8 name (valid on Linux/Unix but not Windows). This must fail
    // rather than being silently mangled into a UTF-8 package path.
    let invalid_name = OsString::from_vec(vec![b'b', b'a', b'd', 0xFF, b'.', b't', b'x', b't']);
    let invalid_path = root.join("subdir").join(invalid_name);
    fs::create_dir_all(invalid_path.parent().unwrap())?;
    fs::write(&invalid_path, b"bad\n")?;

    let out = tempfile::tempdir()?;
    let iso_path = out.path().join("out.iso");

    let err = aero_packager::write_iso9660_joliet_from_dir(root, &iso_path, "TEST_VOL", 0)
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("non-UTF8 path component") && msg.contains("\\xFF"),
        "unexpected error: {msg}"
    );

    Ok(())
}

