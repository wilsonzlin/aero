use std::fs;
use std::io::Read;
use std::path::PathBuf;

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[test]
fn guest_tools_devices_cmd_is_generated_from_device_contract() -> anyhow::Result<()> {
    let packager_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = packager_root.join("..").join("..").join("..");

    let contract_path = repo_root
        .join("docs")
        .join("windows-device-contract.json");
    let devices_cmd_path = repo_root
        .join("guest-tools")
        .join("config")
        .join("devices.cmd");

    let generated = aero_packager::generate_guest_tools_devices_cmd_bytes(&contract_path)?;
    let generated = String::from_utf8(generated)?;
    let existing = fs::read_to_string(&devices_cmd_path)?;

    assert_eq!(
        normalize_newlines(&existing),
        normalize_newlines(&generated),
        "guest-tools/config/devices.cmd does not match generated output from {}",
        contract_path.display()
    );

    Ok(())
}

#[test]
fn virtio_win_packaging_overrides_service_names_in_devices_cmd() -> anyhow::Result<()> {
    let packager_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = packager_root.join("..").join("..").join("..");

    let guest_tools_dir = repo_root
        .join("tools")
        .join("packaging")
        .join("aero_packager")
        .join("testdata")
        .join("guest-tools-no-certs");

    let spec_path = repo_root
        .join("tools")
        .join("packaging")
        .join("specs")
        .join("win7-virtio-win.json");

    let contract_path = repo_root
        .join("docs")
        .join("windows-device-contract.json");

    // Build a minimal virtio-win-style driver tree containing `viostor` and `netkvm`.
    // The packager only validates that expected HWID patterns appear in at least one INF,
    // and that each driver directory contains an .inf/.sys/.cat.
    let drivers_tmp = tempfile::tempdir()?;
    for arch in ["x86", "amd64"] {
        let arch_dir = drivers_tmp.path().join(arch);
        fs::create_dir_all(&arch_dir)?;

        let viostor = arch_dir.join("viostor");
        fs::create_dir_all(&viostor)?;
        fs::write(viostor.join("viostor.inf"), "PCI\\VEN_1AF4&DEV_1042\n")?;
        fs::write(viostor.join("viostor.sys"), b"dummy")?;
        fs::write(viostor.join("viostor.cat"), b"dummy")?;

        let netkvm = arch_dir.join("netkvm");
        fs::create_dir_all(&netkvm)?;
        fs::write(netkvm.join("netkvm.inf"), "PCI\\VEN_1AF4&DEV_1041\n")?;
        fs::write(netkvm.join("netkvm.sys"), b"dummy")?;
        fs::write(netkvm.join("netkvm.cat"), b"dummy")?;
    }

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
        windows_device_contract_path: contract_path,
        out_dir: out_dir.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::None,
        source_date_epoch: 0,
    };
    let outputs = aero_packager::package_guest_tools(&config)?;

    // Confirm that the packager generated a devices.cmd that matches the upstream virtio-win
    // service name, even though the in-repo Windows device contract uses Aero service names.
    let zip_file = fs::File::open(&outputs.zip_path)?;
    let mut zip = zip::ZipArchive::new(zip_file)?;
    let mut entry = zip.by_name("config/devices.cmd")?;
    let mut contents = String::new();
    entry.read_to_string(&mut contents)?;

    assert!(
        contents.contains("set \"AERO_VIRTIO_BLK_SERVICE=viostor\""),
        "expected virtio-win viostor service name override in packaged devices.cmd, got:\n{contents}"
    );
    assert!(
        contents.contains("set \"AERO_VIRTIO_NET_SERVICE=netkvm\""),
        "expected virtio-win netkvm service name override in packaged devices.cmd, got:\n{contents}"
    );

    Ok(())
}
