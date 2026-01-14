use std::fs;

#[test]
fn packaging_fails_fast_on_joliet_identifier_overflow() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    // Use a private copy of the test fixtures so we can mutate them freely.
    let drivers_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("drivers"), drivers_tmp.path())?;

    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&testdata.join("guest-tools"), guest_tools_tmp.path())?;

    // Joliet directory records store `record_len` as a u8 with the formula:
    //   record_len = 33 + id_len + padding
    // For UCS-2BE (Joliet), `id_len` is always even so padding is always 1. The maximum `id_len`
    // is therefore 220 bytes. A filename with 111 ASCII chars encodes to 222 bytes and must be
    // rejected.
    let long_name = format!("{}.txt", "a".repeat(111));
    let long_rel_path = format!("licenses/{long_name}");
    let licenses_dir = guest_tools_tmp.path().join("licenses");
    fs::create_dir_all(&licenses_dir)?;
    fs::write(licenses_dir.join(&long_name), b"dummy\n")?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path: testdata.join("spec.json"),
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("Joliet") && msg.contains(&long_rel_path),
        "unexpected error: {msg}"
    );

    Ok(())
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

fn device_contract_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs")
        .join("windows-device-contract.json")
}

