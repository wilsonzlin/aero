#![cfg(unix)]

use std::fs;

#[test]
fn iso_from_dir_fails_on_symlink() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir()?;
    let root = dir.path();

    let real = root.join("real.txt");
    fs::write(&real, b"real\n")?;

    // Hidden symlink should still fail fast (must not be silently skipped by host-metadata filters).
    let link = root.join(".link.txt");
    symlink(&real, &link)?;

    let out = tempfile::tempdir()?;
    let iso_path = out.path().join("out.iso");

    let err = aero_packager::write_iso9660_joliet_from_dir(root, &iso_path, "TEST_VOL", 0)
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("symlink"), "unexpected error: {msg}");
    assert!(
        msg.contains(&link.display().to_string()),
        "expected error to include full symlink path {}; got: {msg}",
        link.display()
    );

    Ok(())
}
