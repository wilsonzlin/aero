use std::collections::BTreeSet;
use std::fs;

#[test]
fn package_outputs_are_reproducible_and_contain_expected_files() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_dir = testdata.join("guest-tools");
    let spec_path = testdata.join("spec.json");

    let out1 = tempfile::tempdir()?;
    let out2 = tempfile::tempdir()?;

    let config1 = aero_packager::PackageConfig {
        drivers_dir: drivers_dir.clone(),
        guest_tools_dir: guest_tools_dir.clone(),
        out_dir: out1.path().to_path_buf(),
        spec_path: spec_path.clone(),
        version: "1.2.3".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        source_date_epoch: 0,
    };
    let config2 = aero_packager::PackageConfig {
        out_dir: out2.path().to_path_buf(),
        ..config1.clone()
    };

    let outputs1 = aero_packager::package_guest_tools(&config1)?;
    let outputs2 = aero_packager::package_guest_tools(&config2)?;

    // Deterministic outputs: byte-identical ISO/zip/manifest.
    assert_eq!(fs::read(&outputs1.iso_path)?, fs::read(&outputs2.iso_path)?);
    assert_eq!(fs::read(&outputs1.zip_path)?, fs::read(&outputs2.zip_path)?);
    assert_eq!(
        fs::read(&outputs1.manifest_path)?,
        fs::read(&outputs2.manifest_path)?
    );

    // Verify ISO contains expected tree (via Joliet directory records).
    let iso_bytes = fs::read(&outputs1.iso_path)?;
    let tree = aero_packager::read_joliet_tree(&iso_bytes)?;
    for required in [
        "setup.cmd",
        "uninstall.cmd",
        "README.md",
        "manifest.json",
        "certs/test.cer",
        "drivers/x86/testdrv/test.inf",
        "drivers/x86/testdrv/test.sys",
        "drivers/x86/testdrv/test.cat",
        "drivers/amd64/testdrv/test.inf",
        "drivers/amd64/testdrv/test.sys",
        "drivers/amd64/testdrv/test.cat",
    ] {
        assert!(
            tree.contains(required),
            "ISO is missing required file: {required}"
        );
    }

    // Zip should contain the exact same files.
    let zip_file = fs::File::open(&outputs1.zip_path)?;
    let mut zip = zip::ZipArchive::new(zip_file)?;
    let mut zip_paths = BTreeSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        zip_paths.insert(entry.name().to_string());
    }
    assert_eq!(zip_paths, tree.paths);

    Ok(())
}
