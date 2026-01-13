#![cfg(target_os = "linux")]

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

#[test]
fn packaging_fails_on_non_utf8_driver_paths() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    for arch in ["x86", "amd64"] {
        write_stub_pci_driver(
            &drivers_tmp.path().join(arch).join("testdrv"),
            "testdrv",
            r"PCI\VEN_1234&DEV_5678",
        )?;
    }

    // Create an additional file with a non-UTF8 filename (valid on Linux/Unix but not Windows).
    // This must fail packaging rather than being silently mangled into a UTF-8 package path.
    let invalid_name = OsString::from_vec(vec![b'b', b'a', b'd', 0xFF, b'.', b'b', b'i', b'n']);
    let invalid_path = drivers_tmp
        .path()
        .join("x86")
        .join("testdrv")
        .join(invalid_name);
    fs::write(&invalid_path, b"bad\n")?;

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids": [],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("non-UTF8") && msg.contains("testdrv"),
        "unexpected error: {msg}"
    );

    Ok(())
}

#[test]
fn packaging_fails_on_non_utf8_guest_tools_paths() -> anyhow::Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");

    let drivers_dir = testdata.join("drivers");
    let guest_tools_src = testdata.join("guest-tools");
    let guest_tools_tmp = tempfile::tempdir()?;
    copy_dir_all(&guest_tools_src, guest_tools_tmp.path())?;

    // Inject a file with a non-UTF8 filename under guest-tools/config/.
    let invalid_name = OsString::from_vec(vec![b'b', b'a', b'd', 0xFF, b'.', b't', b'x', b't']);
    let invalid_path = guest_tools_tmp.path().join("config").join(invalid_name);
    fs::write(&invalid_path, b"bad\n")?;

    let spec_path = testdata.join("spec.json");
    let out = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir,
        guest_tools_dir: guest_tools_tmp.path().to_path_buf(),
        windows_device_contract_path: device_contract_path(),
        out_dir: out.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    let err = aero_packager::package_guest_tools(&config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("non-UTF8 path component") && msg.contains("\\xFF"),
        "unexpected error: {msg}"
    );

    Ok(())
}

fn write_stub_pci_driver(dir: &Path, base_name: &str, hwid: &str) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(format!("{base_name}.inf")),
        format!(
            concat!(
                "[Version]\n",
                "Signature=\"$Windows NT$\"\n",
                "\n",
                "[Manufacturer]\n",
                "%Mfg%=Models,NTx86,NTamd64\n",
                "\n",
                "[Models.NTx86]\n",
                "%Dev%=Install, {hwid}\n",
                "\n",
                "[Models.NTamd64]\n",
                "%Dev%=Install, {hwid}\n",
                "\n",
                "[Install]\n",
                "CopyFiles=CopyFilesSection\n",
                "\n",
                "[CopyFilesSection]\n",
                "{base_name}.sys\n",
                "\n",
                "[SourceDisksFiles]\n",
                "{base_name}.sys=1\n",
                "\n",
                "[Strings]\n",
                "Mfg=\"Aero\"\n",
                "Dev=\"Test\"\n",
            ),
            hwid = hwid,
            base_name = base_name
        ),
    )?;
    fs::write(dir.join(format!("{base_name}.sys")), b"dummy sys\n")?;
    fs::write(dir.join(format!("{base_name}.cat")), b"dummy cat\n")?;
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
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

fn device_contract_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs")
        .join("windows-device-contract.json")
}
