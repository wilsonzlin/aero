use std::fs;

#[test]
fn packaging_succeeds_for_utf16le_inf_without_bom() -> anyhow::Result<()> {
    let inf_text = make_test_inf_text();
    let inf_bytes = encode_utf16_no_bom(&inf_text, true);
    assert!(nul_ratio(&inf_bytes) < 0.3, "expected <30% NUL bytes");
    assert!(nul_ratio(&inf_bytes) > 0.2, "expected >20% NUL bytes");

    package_with_inf_bytes(&inf_bytes)
}

#[test]
fn packaging_succeeds_for_utf16be_inf_without_bom() -> anyhow::Result<()> {
    let inf_text = make_test_inf_text();
    let inf_bytes = encode_utf16_no_bom(&inf_text, false);
    assert!(nul_ratio(&inf_bytes) < 0.3, "expected <30% NUL bytes");
    assert!(nul_ratio(&inf_bytes) > 0.2, "expected >20% NUL bytes");

    package_with_inf_bytes(&inf_bytes)
}

fn package_with_inf_bytes(inf_bytes: &[u8]) -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = repo_root.join("testdata");
    let guest_tools_dir = testdata.join("guest-tools");

    let drivers_tmp = tempfile::tempdir()?;
    for arch in ["x86", "amd64"] {
        let driver_dir = drivers_tmp.path().join(arch).join("testdrv");
        fs::create_dir_all(&driver_dir)?;

        // Critically: UTF-16, but *no BOM*. Windows tooling frequently produces INFs in this
        // encoding, and the packager should still be able to validate HWIDs inside them.
        fs::write(driver_dir.join("testdrv.inf"), inf_bytes)?;
        fs::write(driver_dir.join("testdrv.sys"), b"dummy sys\n")?;
        fs::write(driver_dir.join("testdrv.cat"), b"dummy cat\n")?;
    }

    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            }
        ]
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let out_dir = tempfile::tempdir()?;
    let config = aero_packager::PackageConfig {
        drivers_dir: drivers_tmp.path().to_path_buf(),
        guest_tools_dir,
        windows_device_contract_path: device_contract_path(),
        out_dir: out_dir.path().to_path_buf(),
        spec_path,
        version: "0.0.0".to_string(),
        build_id: "test".to_string(),
        volume_id: "AERO_GUEST_TOOLS".to_string(),
        signing_policy: aero_packager::SigningPolicy::Test,
        source_date_epoch: 0,
    };

    aero_packager::package_guest_tools(&config)?;
    Ok(())
}

fn make_test_inf_text() -> String {
    let hwid = r"PCI\VEN_1234&DEV_5678";
    let base = format!(
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
            "[Install.Services]\n",
            "AddService = TestSvc,0x00000002,Service_Inst\n",
            "\n",
            "[Service_Inst]\n",
            "ServiceType=1\n",
            "StartType=3\n",
            "ErrorControl=1\n",
            "ServiceBinary=%12%\\testdrv.sys\n",
            "\n",
            "[CopyFilesSection]\n",
            "testdrv.sys\n",
            "\n",
            "[SourceDisksFiles]\n",
            "testdrv.sys=1\n",
            "\n",
            "[Strings]\n",
            "Mfg=\"Aero\"\n",
            "Dev=\"Test\"\n",
        ),
        hwid = hwid
    );

    // Ensure the overall file isn't "too ASCII-heavy" by appending a large non-ASCII string.
    // This is still a valid INF, but reduces the NUL-byte ratio below the strict `>= 30%` rule.
    let unicode_payload = "Ω".repeat(base.len());
    format!("{base}ExtraDesc=\"{unicode_payload}\"\n")
}

fn encode_utf16_no_bom(text: &str, little_endian: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        if little_endian {
            bytes.extend_from_slice(&unit.to_le_bytes());
        } else {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
    }
    bytes
}

fn nul_ratio(bytes: &[u8]) -> f64 {
    let nul_count = bytes.iter().filter(|b| **b == 0).count();
    nul_count as f64 / bytes.len() as f64
}

fn device_contract_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("docs")
        .join("windows-device-contract.json")
}
