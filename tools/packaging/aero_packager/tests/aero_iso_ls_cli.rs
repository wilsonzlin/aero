use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

#[test]
fn aero_iso_ls_cli_lists_joliet_file_paths() -> anyhow::Result<()> {
    let input = tempfile::tempdir()?;
    let root = input.path();

    fs::write(root.join("a.txt"), b"a\n")?;
    fs::create_dir_all(root.join("dir"))?;
    fs::write(root.join("dir").join("b.txt"), b"b\n")?;

    let out = tempfile::tempdir()?;
    let iso = out.path().join("out.iso");
    aero_packager::write_iso9660_joliet_from_dir(root, &iso, "TEST_VOL", 0)?;

    let bin = env!("CARGO_BIN_EXE_aero_iso_ls");
    let proc = Command::new(bin).arg("--iso").arg(&iso).output()?;
    assert!(
        proc.status.success(),
        "aero_iso_ls exited non-zero: {}",
        String::from_utf8_lossy(&proc.stderr)
    );

    let lines: BTreeSet<String> = String::from_utf8_lossy(&proc.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let expected: BTreeSet<String> = ["/a.txt", "/dir/b.txt"]
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    assert_eq!(lines, expected);

    Ok(())
}

