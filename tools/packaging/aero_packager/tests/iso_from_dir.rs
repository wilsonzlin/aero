use std::fs;

#[test]
fn iso_from_dir_is_deterministic_and_filters_host_metadata() -> anyhow::Result<()> {
    let input = tempfile::tempdir()?;
    let root = input.path();

    fs::write(root.join("b.txt"), b"b\n")?;
    fs::write(root.join("a.txt"), b"a\n")?;
    fs::create_dir_all(root.join("dir1"))?;
    fs::write(root.join("dir1").join("c.txt"), b"c\n")?;

    // Host metadata / hidden files that must not affect the output ISO.
    fs::write(root.join(".DS_Store"), b"junk")?;
    fs::create_dir_all(root.join(".git"))?;
    fs::write(root.join(".git").join("config"), b"junk")?;
    fs::create_dir_all(root.join("__MACOSX"))?;
    fs::write(root.join("__MACOSX").join("junk"), b"junk")?;
    fs::write(root.join("Thumbs.db"), b"junk")?;
    fs::write(root.join("desktop.ini"), b"junk")?;

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;
    let iso1 = out1.path().join("out.iso");
    let iso2 = out2.path().join("out.iso");

    aero_packager::write_iso9660_joliet_from_dir(root, &iso1, "TEST_VOL", 0)?;
    aero_packager::write_iso9660_joliet_from_dir(root, &iso2, "TEST_VOL", 0)?;

    let bytes1 = fs::read(&iso1)?;
    let bytes2 = fs::read(&iso2)?;
    assert_eq!(bytes1, bytes2, "ISO bytes differed across identical runs");

    let tree = aero_packager::read_joliet_tree(&bytes1)?;
    for expected in ["a.txt", "b.txt", "dir1/c.txt"] {
        assert!(tree.contains(expected), "ISO is missing {expected}");
    }
    for unexpected in [
        ".DS_Store",
        ".git/config",
        "__MACOSX/junk",
        "Thumbs.db",
        "desktop.ini",
    ] {
        assert!(
            !tree.contains(unexpected),
            "ISO unexpectedly contains filtered file: {unexpected}"
        );
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn iso_from_dir_rejects_case_insensitive_colliding_files() -> anyhow::Result<()> {
    let input = tempfile::tempdir()?;
    let root = input.path();

    fs::write(root.join("Foo.txt"), b"foo\n")?;
    fs::write(root.join("foo.txt"), b"bar\n")?;

    let out = tempfile::tempdir()?;
    let iso = out.path().join("out.iso");

    let err = aero_packager::write_iso9660_joliet_from_dir(root, &iso, "TEST_VOL", 0).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("case-insensitive path collision"),
        "unexpected error: {msg}"
    );
    assert!(msg.contains("foo.txt"), "unexpected error: {msg}");
    assert!(msg.contains("Foo.txt"), "unexpected error: {msg}");

    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn iso_from_dir_rejects_case_insensitive_colliding_dirs() -> anyhow::Result<()> {
    let input = tempfile::tempdir()?;
    let root = input.path();

    fs::create_dir_all(root.join("Drivers").join("A"))?;
    fs::create_dir_all(root.join("drivers").join("a"))?;

    fs::write(root.join("Drivers").join("A").join("one.txt"), b"one\n")?;
    fs::write(root.join("drivers").join("a").join("two.txt"), b"two\n")?;

    let out = tempfile::tempdir()?;
    let iso = out.path().join("out.iso");

    let err = aero_packager::write_iso9660_joliet_from_dir(root, &iso, "TEST_VOL", 0).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("case-insensitive path collision"),
        "unexpected error: {msg}"
    );
    // Ensure the collision report includes the implied directories.
    assert!(msg.contains("drivers/a"), "unexpected error: {msg}");
    assert!(msg.contains("Drivers/A/"), "unexpected error: {msg}");
    assert!(msg.contains("drivers/a/"), "unexpected error: {msg}");

    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn iso_from_dir_fails_on_non_utf8_paths() -> anyhow::Result<()> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let input = tempfile::tempdir()?;
    let root = input.path();

    fs::write(root.join("ok.txt"), b"ok\n")?;

    let invalid_name = OsString::from_vec(vec![b'b', b'a', b'd', 0xFF, b'.', b'b', b'i', b'n']);
    fs::write(root.join(invalid_name), b"bad\n")?;

    let out = tempfile::tempdir()?;
    let iso = out.path().join("out.iso");

    let err = aero_packager::write_iso9660_joliet_from_dir(root, &iso, "TEST_VOL", 0).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("non-UTF8") && msg.contains("\\xFF"),
        "unexpected error: {msg}"
    );

    Ok(())
}
