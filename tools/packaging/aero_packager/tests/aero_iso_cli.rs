use std::fs;
use std::process::Command;

#[test]
fn aero_iso_cli_source_date_epoch_env_and_arg_are_deterministic() -> anyhow::Result<()> {
    let input = tempfile::tempdir()?;
    fs::write(input.path().join("a.txt"), b"a\n")?;
    fs::create_dir_all(input.path().join("dir"))?;
    fs::write(input.path().join("dir").join("b.txt"), b"b\n")?;

    let bin = env!("CARGO_BIN_EXE_aero_iso");

    // Env var path: repeated builds should be identical when SOURCE_DATE_EPOCH is fixed.
    let out1 = tempfile::tempdir()?;
    let iso1 = out1.path().join("out.iso");
    let status = Command::new(bin)
        .arg("--in-dir")
        .arg(input.path())
        .arg("--out-iso")
        .arg(&iso1)
        .arg("--volume-id")
        .arg("TEST_VOL")
        .env("SOURCE_DATE_EPOCH", "123")
        .status()?;
    assert!(status.success());

    let out2 = tempfile::tempdir()?;
    let iso2 = out2.path().join("out.iso");
    let status = Command::new(bin)
        .arg("--in-dir")
        .arg(input.path())
        .arg("--out-iso")
        .arg(&iso2)
        .arg("--volume-id")
        .arg("TEST_VOL")
        .env("SOURCE_DATE_EPOCH", "123")
        .status()?;
    assert!(status.success());
    assert_eq!(fs::read(&iso1)?, fs::read(&iso2)?);

    // Explicit arg should override differing SOURCE_DATE_EPOCH env values.
    let out3 = tempfile::tempdir()?;
    let iso3 = out3.path().join("out.iso");
    let status = Command::new(bin)
        .arg("--in-dir")
        .arg(input.path())
        .arg("--out-iso")
        .arg(&iso3)
        .arg("--volume-id")
        .arg("TEST_VOL")
        .arg("--source-date-epoch")
        .arg("456")
        .env("SOURCE_DATE_EPOCH", "999")
        .status()?;
    assert!(status.success());

    let out4 = tempfile::tempdir()?;
    let iso4 = out4.path().join("out.iso");
    let status = Command::new(bin)
        .arg("--in-dir")
        .arg(input.path())
        .arg("--out-iso")
        .arg(&iso4)
        .arg("--volume-id")
        .arg("TEST_VOL")
        .arg("--source-date-epoch")
        .arg("456")
        .env("SOURCE_DATE_EPOCH", "111")
        .status()?;
    assert!(status.success());

    assert_eq!(fs::read(&iso3)?, fs::read(&iso4)?);

    // And a different timestamp should affect the ISO bytes.
    assert_ne!(fs::read(&iso1)?, fs::read(&iso3)?);

    Ok(())
}

